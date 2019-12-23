use std::cmp;
use std::fmt;
use std::future::Future;
use std::io::Read;
use std::io;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use blocking_permit::{
    blocking_permit_future, SyncBlockingPermitFuture,
    dispatch_rx, Dispatched,
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_core::stream::Stream;
use olio::fs::rc::ReadSlice;
use tao_log::{debug, warn};

use body_image::{BodyImage, ExplodedImage, Prolog, };

use crate::{
    FutioTunables, RequestRecord, RequestRecorder, StreamWrapper, UniBodyBuf
};

#[cfg(feature = "mmap")] use memmap::Mmap;
#[cfg(feature = "mmap")] use crate::MemMapBuf;
#[cfg(feature = "mmap")] use olio::mem::{MemAdvice, MemHandle};
#[cfg(feature = "mmap")] use body_image::_mem_handle_ext::MemHandleExt;

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum Blocking {
    // Not requested nor allowed
    Void,
    // Requested Once
    Pending,
    // One blocking operation granted
    Once,
    // Any/all blocking operations perpetually granted
    Always,
}

pub trait BlockingArbiter {
    fn can_block(&mut self) -> bool;

    fn state(&self) -> Blocking;
}

#[derive(Debug, Default)]
pub struct LenientArbiter;

impl BlockingArbiter for LenientArbiter {
    fn can_block(&mut self) -> bool {
        true
    }

    fn state(&self) -> Blocking {
        Blocking::Always
    }
}

#[derive(Debug)]
pub struct StatefulArbiter {
    state: Blocking
}

impl Default for StatefulArbiter {
    fn default() -> Self {
        StatefulArbiter { state: Blocking::Void }
    }
}

impl StatefulArbiter {
    pub(crate) fn set(&mut self, state: Blocking) {
        self.state = state;
    }
}

impl BlockingArbiter for StatefulArbiter {
    // Return true if blocking is allowed, consuming any one-time
    // allowance. Otherwise records that blocking has been requested.
    fn can_block(&mut self) -> bool {
        match self.state {
            Blocking::Void => {
                self.state = Blocking::Pending;
                false
            }
            Blocking::Pending => {
                false
            }
            Blocking::Once => {
                self.state = Blocking::Void;
                true
            }
            Blocking::Always => {
                true
            }
        }
    }

    fn state(&self) -> Blocking {
        self.state
    }
}

/// Trait for satisftying Stream output buffer requirments.
pub trait OutputBuf: Buf + 'static + From<Bytes> + Send + Sync + Unpin {
    /// Convert from a mmap.
    #[cfg(feature = "mmap")]
    fn from_mmap(mmap: MemHandle<Mmap>) -> Result<Self, io::Error>;
}

impl OutputBuf for Bytes {
    #[cfg(feature = "mmap")]
    fn from_mmap(mmap: MemHandle<Mmap>) -> Result<Self, io::Error> {
        match mmap.tmp_advise(
            MemAdvice::Sequential,
            || Ok(Bytes::copy_from_slice(&mmap[..])))
        {
            Ok(b) => {
                debug!("MemMap copied to Bytes (len: {})", b.len());
                Ok(b)
            }
            Err(e) => Err(e),
        }

    }
}

impl OutputBuf for UniBodyBuf {
    #[cfg(feature = "mmap")]
    fn from_mmap(mmap: MemHandle<Mmap>) -> Result<Self, io::Error> {
        let buf = MemMapBuf::new(mmap);
        buf.advise_sequential()?;
        let _b = buf.bytes()[0];
        debug!("MemMap prepared for sequential read (len: {})", buf.remaining());
        Ok(UniBodyBuf::from_mmap(buf))
    }

}

/// Adaptor for `BodyImage` implementing the `futures::Stream` and
/// `http_body::Body` traits, using the custom
/// [`UniBodyBuf`](struct.UniBodyBuf.html) item buffer type (instead of
/// `Bytes`) for zero-copy `MemMap` support (*mmap* feature only).
///
/// The `HttpBody` trait (plus `Send`) makes this usable with hyper as the `B`
/// body type of `http::Request<B>` (client) or `http::Response<B>`
/// (server).
///
/// `Tunables::buffer_size_fs` is used for reading the body when in `FsRead`
/// state. `BodyImage` in `Ram` is made available with zero-copy using a
/// consuming iterator.  This implementation uses permits or a dispatch pool
/// for blocking reads from `FsRead` state and when dereferencing from `MemMap`
/// state.
#[derive(Debug)]
pub struct OmniBodyImage<B, BA=LenientArbiter>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    state: OmniBodyState,
    len: u64,
    consumed: u64,
    tune: FutioTunables,
    arbiter: BA,
    buf_type: PhantomData<fn() -> B>,
}

impl<B, BA> OmniBodyImage<B, BA>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` and `FutioTunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    pub fn new(body: BodyImage, tune: FutioTunables) -> Self {
        let len = body.len();
        match body.explode() {
            ExplodedImage::Ram(v) => {
                OmniBodyImage {
                    state: OmniBodyState::Ram(v.into_iter()),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                    buf_type: PhantomData,
                }
            }
            ExplodedImage::FsRead(rs) => {
                OmniBodyImage {
                    state: OmniBodyState::File(rs),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                    buf_type: PhantomData,
                }
            }
            #[cfg(feature = "mmap")]
            ExplodedImage::MemMap(mmap) => {
                OmniBodyImage {
                    state: OmniBodyState::MemMap(Some(mmap)),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                    buf_type: PhantomData,
                }
            }
        }
    }

    fn poll_impl(&mut self) -> Poll<Option<<Self as Stream>::Item>> {
        let avail = self.len - self.consumed;
        if avail == 0 {
            return Poll::Ready(None);
        }

        // All states besides Ram require blocking
        match self.state {
            OmniBodyState::Ram(_) => {}
            _ => {
                if !self.arbiter.can_block() {
                    return Poll::Pending;
                }
            }
        }

        match self.state {
            OmniBodyState::Ram(ref mut iter) => {
                match iter.next() {
                    Some(b) => {
                        self.consumed += b.len() as u64;
                        Poll::Ready(Some(Ok(b.into())))
                    }
                    None => Poll::Ready(None),
                }
            }
            OmniBodyState::File(ref mut rs) => {
                let rlen = cmp::min(
                    self.tune.image().buffer_size_fs() as u64,
                    avail
                ) as usize;
                let mut buf = BytesMut::with_capacity(rlen);
                let b = unsafe {
                    &mut *(buf.bytes_mut()
                           as *mut [mem::MaybeUninit<u8>] as *mut [u8])
                };
                loop {
                    match rs.read(&mut b[..rlen]) {
                        Ok(0) => break Poll::Ready(None),
                        Ok(len) => {
                            unsafe { buf.advance_mut(len); }
                            debug!("read chunk (len: {})", len);
                            self.consumed += len as u64;
                            break Poll::Ready(Some(Ok(
                                buf.freeze().into()
                            )));
                        }
                        Err(e) => {
                            if e.kind() == io::ErrorKind::Interrupted {
                                warn!("OmniBodyImage: write interrupted");
                            } else {
                                break Poll::Ready(Some(Err(e)));
                            }
                        }
                    }
                }
            }
            #[cfg(feature = "mmap")]
            OmniBodyState::MemMap(ref mut ob) => {
                if let Some(mb) = ob.take() {
                    match B::from_mmap(mb) {
                        Ok(buf) => {
                            self.consumed += buf.remaining() as u64;
                            Poll::Ready(Some(Ok(buf)))
                        }
                        Err(e) => Poll::Ready(Some(Err(e))),
                    }
                } else {
                    Poll::Ready(None)
                }
            }
        }
    }
}

impl<B, BA> StreamWrapper for OmniBodyImage<B, BA>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        OmniBodyImage::new(body, tune)
    }
}

enum OmniBodyState {
    Ram(IntoIter<Bytes>),
    File(ReadSlice),
    #[cfg(feature = "mmap")]
    MemMap(Option<MemHandle<Mmap>>),
}

impl fmt::Debug for OmniBodyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            OmniBodyState::Ram(_) => {
                // Avoids showing all buffers as u8 lists
                write!(f, "Ram(IntoIter<Bytes>)")
            }
            OmniBodyState::File(ref rs) => {
                f.debug_struct("File")
                    .field("rs", rs)
                    .finish()
            }
            #[cfg(feature = "mmap")]
            OmniBodyState::MemMap(ref ob) => {
                f.debug_tuple("MemMap")
                    .field(ob)
                    .finish()
            }
        }
    }
}

impl<B, BA> Stream for OmniBodyImage<B, BA>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    type Item = Result<B, io::Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        self.get_mut().poll_impl()
    }
}

impl<B, BA> http_body::Body for OmniBodyImage<B, BA>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    type Data = B;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        self.poll_next(cx)
    }

    fn poll_trailers(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Result<Option<http::HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.len)
    }

    fn is_end_stream(&self) -> bool {
        self.consumed >= self.len
    }
}

pub struct PermitBodyImage<B>
    where B: OutputBuf
{
    image: OmniBodyImage<B, StatefulArbiter>,
    permit: Option<SyncBlockingPermitFuture<'static>>
}

impl<B> StreamWrapper for PermitBodyImage<B>
    where B: OutputBuf
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        PermitBodyImage {
            image: OmniBodyImage::new(body, tune),
            permit: None
        }
    }
}

impl<B> Stream for PermitBodyImage<B>
    where B: OutputBuf
{
    type Item = Result<B, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        let this = self.get_mut();
        let permit = if let Some(ref mut pf) = this.permit {
            let pf = Pin::new(pf);
            match pf.poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(p)) => {
                    this.permit = None;
                    Some(p)
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
            }
        } else {
            None
        };

        if let Some(p) = permit {
            this.image.arbiter.set(Blocking::Once);
            let image = Pin::new(&mut this.image);
            let res = p.run(|| image.poll_next(cx));
            debug_assert_eq!(this.image.arbiter.state(), Blocking::Void);
            res
        } else {
            let res = {
                let image = Pin::new(&mut this.image);
                image.poll_next(cx)
            };

            if res.is_pending()
                && this.image.arbiter.state() == Blocking::Pending
            {
                this.permit = Some(blocking_permit_future(
                    this.image.tune.blocking_semaphore()
                        .expect("blocking semaphore required!")
                ).make_sync());
            }
            res
        }
    }
}

impl<B> http_body::Body for PermitBodyImage<B>
    where B: OutputBuf,
{
    type Data = B;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        self.poll_next(cx)
    }

    fn poll_trailers(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Result<Option<http::HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::Body::size_hint(&self.image)
    }

    fn is_end_stream(&self) -> bool {
        http_body::Body::is_end_stream(&self.image)
    }
}

pub struct DispatchBodyImage<B>
    where B: OutputBuf
{
    state: DispatchState<B>,
    len: u64,
}

enum DispatchState<B>
    where B: OutputBuf
{
    Image(Option<OmniBodyImage<B, StatefulArbiter>>),
    Dispatch(Dispatched<(
        Poll<Option<Result<B, io::Error>>>,
        OmniBodyImage<B, StatefulArbiter>)>),
}

impl<B> StreamWrapper for DispatchBodyImage<B>
    where B: OutputBuf
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        let len = body.len();
        DispatchBodyImage {
            state: DispatchState::Image(Some(OmniBodyImage::new(body, tune))),
            len,
        }
    }
}

impl<B> Stream for DispatchBodyImage<B>
    where B: OutputBuf
{
    type Item = Result<B, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        let this = self.get_mut();
        match this.state {
            DispatchState::Image(ref mut obo) => {
                let ob = obo.as_mut().unwrap();
                let res = ob.poll_impl();
                if res.is_pending() && ob.arbiter.state() == Blocking::Pending {
                    let mut ob = obo.take().unwrap();
                    this.state = DispatchState::Dispatch(dispatch_rx(move || {
                        (ob.poll_impl(), ob)
                    }).unwrap());
                }
                res
            }
            DispatchState::Dispatch(ref mut db) => {
                let (res, ob) = match Pin::new(&mut *db).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                    Poll::Ready(Ok((res, ob))) => (res, ob),
                };
                debug_assert_eq!(ob.arbiter.state(), Blocking::Void);
                this.state = DispatchState::Image(Some(ob));
                res
            }
        }
    }
}

impl<B> http_body::Body for DispatchBodyImage<B>
    where B: OutputBuf,
{
    type Data = B;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        self.poll_next(cx)
    }

    fn poll_trailers(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Result<Option<http::HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.len)
    }

    fn is_end_stream(&self) -> bool {
        match self.state {
            DispatchState::Image(ref obi) => {
                http_body::Body::is_end_stream(obi.as_ref().unwrap())
            }
            DispatchState::Dispatch(_) => {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_send<T: Send>() -> bool { true }
    fn is_sync<T: Sync>() -> bool { true }

    #[test]
    fn test_send_sync() {
        // In order for OmniBodyImage to work with hyper::Body::wrap_stream,
        // it must be both Sync and Send
        assert!(is_send::<OmniBodyImage<Bytes>>());
        assert!(is_sync::<OmniBodyImage<Bytes>>());

        assert!(is_send::<DispatchBodyImage<Bytes>>());
        assert!(is_sync::<DispatchBodyImage<Bytes>>());

        assert!(is_send::<PermitBodyImage<Bytes>>());
        assert!(is_sync::<PermitBodyImage<Bytes>>());
    }
}
