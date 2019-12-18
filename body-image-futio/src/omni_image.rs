use std::cmp;
use std::fmt;
use std::future::Future;
use std::io::Read;
use std::io;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use blocking_permit::{
    blocking_permit_future, BlockingPermitFuture,
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

#[cfg(feature = "mmap")] use crate::MemMapBuf;

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
    fn set(&mut self, state: Blocking) {
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
pub struct OmniBodyImage<BA=LenientArbiter>
    where BA: BlockingArbiter + Default + Unpin
{
    state: OmniBodyState,
    len: u64,
    consumed: u64,
    tune: FutioTunables,
    arbiter: BA,
}

impl<BA> OmniBodyImage<BA>
    where BA: BlockingArbiter + Default + Unpin
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
                }
            }
            ExplodedImage::FsRead(rs) => {
                OmniBodyImage {
                    state: OmniBodyState::File(rs),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                }
            }
            #[cfg(feature = "mmap")]
            ExplodedImage::MemMap(mmap) => {
                OmniBodyImage {
                    state: OmniBodyState::MemMap(Some(MemMapBuf::new(mmap))),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                }
            }
        }
    }

    fn poll_impl(&mut self) -> Poll<Option<<Self as Stream>::Item>>
    {
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
                        Poll::Ready(Some(Ok(UniBodyBuf::from_bytes(b))))
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
                                UniBodyBuf::from_bytes(buf.freeze())
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
                    mb.advise_sequential()?;
                    let _b = mb.bytes()[0];
                    self.consumed += mb.len() as u64;
                    debug!("prepared MemMap (len: {})", mb.len());
                    Poll::Ready(Some(Ok(UniBodyBuf::from_mmap(mb))))
                } else {
                    Poll::Ready(None)
                }
            }
        }
    }
}

impl<BA> StreamWrapper for OmniBodyImage<BA>
    where BA: BlockingArbiter + Default + Unpin
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        OmniBodyImage::new(body, tune)
    }
}

enum OmniBodyState {
    Ram(IntoIter<Bytes>),
    File(ReadSlice),
    #[cfg(feature = "mmap")]
    MemMap(Option<MemMapBuf>),
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

// FIXME: Make this generic over the Bytes/UniBodyBuf?

impl<BA> Stream for OmniBodyImage<BA>
    where BA: BlockingArbiter + Default + Unpin
{
    type Item = Result<UniBodyBuf, io::Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        self.get_mut().poll_impl()
    }
}

impl<BA> http_body::Body for OmniBodyImage<BA>
    where BA: BlockingArbiter + Default + Unpin
{
    type Data = UniBodyBuf;
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

pub struct PermitWrapper {
    image: OmniBodyImage<StatefulArbiter>,
    permit: Option<BlockingPermitFuture<'static>>
}

impl Stream for PermitWrapper {
    type Item = Result<UniBodyBuf, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        let this = unsafe { self.get_unchecked_mut() };
        let permit = if let Some(ref mut pf) = this.permit {
            let pf = unsafe { Pin::new_unchecked(pf) };
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
            let image = unsafe { Pin::new_unchecked(&mut this.image) };
            let res = p.run(|| image.poll_next(cx));
            debug_assert_eq!(this.image.arbiter.state(), Blocking::Void);
            res
        } else {
            let res = {
                let image = unsafe { Pin::new_unchecked(&mut this.image) };
                image.poll_next(cx)
            };

            if res.is_pending()
                && this.image.arbiter.state() == Blocking::Pending
            {
                this.permit = Some(blocking_permit_future(
                    this.image.tune.blocking_semaphore()
                        .expect("blocking semaphore required!")
                ));
            }
            res
        }
    }
}

pub struct DispatchWrapper {
    state: DispatchState
}

enum DispatchState {
    Image(Option<OmniBodyImage<StatefulArbiter>>),
    Dispatch(Dispatched<(
        Poll<Option<Result<UniBodyBuf, io::Error>>>,
        OmniBodyImage<StatefulArbiter>)>),
}

impl Stream for DispatchWrapper {
    type Item = Result<UniBodyBuf, io::Error>;

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
