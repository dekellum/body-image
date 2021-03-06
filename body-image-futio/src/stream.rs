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
    Cleaver, Splittable, YieldStream,
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_core::stream::Stream;
use olio::fs::rc::ReadSlice;
use tao_log::{debug, warn};

use body_image::{BodyImage, ExplodedImage};

use crate::{
    Blocking, BlockingArbiter, LenientArbiter, StatefulArbiter,
    FutioTunables, StreamWrapper, UniBodyBuf
};

#[cfg(feature = "mmap")]
use {
    memmap::Mmap,
    olio::mem::{MemAdvice, MemHandle},

    body_image::_mem_handle_ext::MemHandleExt,

    crate::MemMapBuf,
};

/// Trait qualifying `Stream` Item-type buffer requirments.
pub trait OutputBuf: Buf + 'static + From<Bytes> + Send + Sync + Unpin {
    /// Convert from a `MemHandle<Mmap>`
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
        let _b = buf.chunk()[0];
        debug!("MemMap prepared for sequential read (len: {})", buf.remaining());
        Ok(UniBodyBuf::from_mmap(buf))
    }

}

/// Adaptor for `BodyImage`, implementing the `futures::Stream` and
/// `http_body::Body` traits.
///
/// The `http_body::Body` trait makes this usable with hyper as the `B`
/// body type of `http::Request<B>` (client) or `http::Response<B>`
/// (server).
///
/// The stream `Item` type is an `OutputBuf`, implemented here for `Bytes` or
/// [`UniBodyBuf`](crate::UniBodyBuf). The later provides zero-copy `MemMap`
/// support (*mmap feature only).
///
/// `Tunables::buffer_size_fs` is used for reading the body when in `FsRead`
/// state.
///
/// See also [`YieldBodyImage`], [`SplitBodyImage`], [`DispatchBodyImage`] and
/// [`PermitBodyImage`] which provide additional coordination of blocking
/// operations.
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct AsyncBodyImage<B, BA=LenientArbiter>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    state: AsyncBodyState,
    len: u64,
    consumed: u64,
    tune: FutioTunables,
    arbiter: BA,
    buf_type: PhantomData<fn() -> B>,
}

impl<B, BA> AsyncBodyImage<B, BA>
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
                AsyncBodyImage {
                    state: AsyncBodyState::Ram(v.into_iter()),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                    buf_type: PhantomData,
                }
            }
            ExplodedImage::FsRead(rs) => {
                AsyncBodyImage {
                    state: AsyncBodyState::File(rs),
                    len,
                    consumed: 0,
                    tune,
                    arbiter: BA::default(),
                    buf_type: PhantomData,
                }
            }
            #[cfg(feature = "mmap")]
            ExplodedImage::MemMap(mmap) => {
                AsyncBodyImage {
                    state: AsyncBodyState::MemMap(Some(mmap)),
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
            AsyncBodyState::Ram(_) => {}
            _ => {
                if !self.arbiter.can_block() {
                    return Poll::Pending;
                }
            }
        }

        match self.state {
            AsyncBodyState::Ram(ref mut iter) => {
                match iter.next() {
                    Some(b) => {
                        self.consumed += b.len() as u64;
                        Poll::Ready(Some(Ok(b.into())))
                    }
                    None => Poll::Ready(None),
                }
            }
            AsyncBodyState::File(ref mut rs) => {
                let rlen = cmp::min(
                    self.tune.image().buffer_size_fs() as u64,
                    avail
                ) as usize;
                let mut buf = BytesMut::with_capacity(rlen);
                let b = unsafe { &mut *(
                    buf.chunk_mut() as *mut _
                        as *mut [mem::MaybeUninit<u8>]
                        as *mut [u8]
                )};
                loop {
                    match rs.read(&mut b[..rlen]) {
                        Ok(0) => break Poll::Ready(None),
                        Ok(len) => {
                            unsafe { buf.advance_mut(len); }
                            debug!("read chunk (len: {})", len);
                            self.consumed += len as u64;
                            break Poll::Ready(Some(Ok(buf.freeze().into())));
                        }
                        Err(e) => {
                            if e.kind() == io::ErrorKind::Interrupted {
                                warn!("AsyncBodyImage: read interrupted");
                            } else {
                                break Poll::Ready(Some(Err(e)));
                            }
                        }
                    }
                }
            }
            #[cfg(feature = "mmap")]
            AsyncBodyState::MemMap(ref mut ob) => {
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

impl<B, BA> StreamWrapper for AsyncBodyImage<B, BA>
    where B: OutputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        AsyncBodyImage::new(body, tune)
    }
}

/// Extends [`AsyncBodyImage`] by splitting buffers and yielding.
///
/// Extends [`AsyncBodyImage`] by splitting stream item buffers to a maximum
/// [`FutioTunables::stream_item_size`] *and* yielding after each each
/// item. This may be effective when the underlying `AsyncBodyImage` contains a
/// vary large contiguous memory region, e.g. after it was gathered or memory
/// mapped, which could cause a large delay when subsequently processed.
pub type SplitBodyImage<B> =
    YieldStream<Cleaver<B, io::Error, AsyncBodyImage<B>>,
                Result<B, io::Error>>;

impl<B> StreamWrapper for SplitBodyImage<B>
    where B: OutputBuf + Splittable
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        let max = tune.stream_item_size();
        YieldStream::new(
            Cleaver::new(AsyncBodyImage::new(body, tune), max)
        )
    }
}

/// Extends [`AsyncBodyImage`] by periodically yielding.
///
/// Extends [`AsyncBodyImage`] by always yielding from `Stream::poll_next` (by
/// returning `Poll::Pending`) immediately after it has returned
/// `Poll::Ready(Some(_))`. This may be effective in some settings since the
/// underlying `AsyncBodyImage` may use blocking reads (directly, without other
/// coordination) and some use cases (e.g. `Stream::fold`) do not otherwise
/// yield.
pub type YieldBodyImage<B> =
    YieldStream<AsyncBodyImage<B>, Result<B, io::Error>>;

impl<B> StreamWrapper for YieldBodyImage<B>
    where B: OutputBuf
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        YieldStream::new(AsyncBodyImage::new(body, tune))
    }
}

enum AsyncBodyState {
    Ram(IntoIter<Bytes>),
    File(ReadSlice),
    #[cfg(feature = "mmap")]
    MemMap(Option<MemHandle<Mmap>>),
}

impl fmt::Debug for AsyncBodyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            AsyncBodyState::Ram(_) => {
                // Avoids showing all buffers as u8 lists
                write!(f, "Ram(IntoIter<Bytes>)")
            }
            AsyncBodyState::File(ref rs) => {
                f.debug_struct("File")
                    .field("rs", rs)
                    .finish()
            }
            #[cfg(feature = "mmap")]
            AsyncBodyState::MemMap(ref ob) => {
                f.debug_tuple("MemMap")
                    .field(ob)
                    .finish()
            }
        }
    }
}

impl<B, BA> Stream for AsyncBodyImage<B, BA>
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

impl<B, BA> http_body::Body for AsyncBodyImage<B, BA>
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

/// Extends [`AsyncBodyImage`] by acquiring a blocking permit before performing
/// any blocking file read operations.
///
/// The total number of concurrent blocking operations is constrained by the
/// `Semaphore` referenced in
/// [`BlockingPolicy::Permit`](crate::BlockingPolicy::Permit) from
/// [`FutioTunables::blocking_policy`], which is required.
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct PermitBodyImage<B>
    where B: OutputBuf
{
    image: AsyncBodyImage<B, StatefulArbiter>,
    permit: Option<SyncBlockingPermitFuture<'static>>
}

impl<B> StreamWrapper for PermitBodyImage<B>
    where B: OutputBuf
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        PermitBodyImage {
            image: AsyncBodyImage::new(body, tune),
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

                // Recurse for correct waking
                return Pin::new(this).poll_next(cx);
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

/// Extends [`AsyncBodyImage`] by further dispatching any blocking file read
/// operations to a [`blocking_permit::DispatchPool`] registered with the
/// current thread.
///
/// The implementation will panic if a `DispatchPool` is not registered. Note
/// that each instance will have, at most, 1 pending dispatched read operation
/// `Future`, that it drives to completion before making new reads.
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct DispatchBodyImage<B>
    where B: OutputBuf
{
    state: DispatchState<B>,
    len: u64,
}

type DispatchReturn<B> = (
    Poll<Option<Result<B, io::Error>>>,
    AsyncBodyImage<B, StatefulArbiter>
);

#[derive(Debug)]
enum DispatchState<B>
    where B: OutputBuf
{
    Image(Option<AsyncBodyImage<B, StatefulArbiter>>),
    Dispatch(Dispatched<DispatchReturn<B>>),
}

impl<B> StreamWrapper for DispatchBodyImage<B>
    where B: OutputBuf
{
    fn new(body: BodyImage, tune: FutioTunables) -> Self {
        let len = body.len();
        DispatchBodyImage {
            state: DispatchState::Image(Some(AsyncBodyImage::new(body, tune))),
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
                    ob.arbiter.set(Blocking::Once);
                    let mut ob = obo.take().unwrap();
                    this.state = DispatchState::Dispatch(dispatch_rx(move || {
                        (ob.poll_impl(), ob)
                    }).unwrap());

                    // Recurse for correct waking
                    return Pin::new(this).poll_next(cx);
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
        // In order for AsyncBodyImage to work with hyper::Body::wrap_stream,
        // it must be both Sync and Send
        assert!(is_send::<AsyncBodyImage<Bytes>>());
        assert!(is_sync::<AsyncBodyImage<Bytes>>());

        assert!(is_send::<DispatchBodyImage<Bytes>>());
        assert!(is_sync::<DispatchBodyImage<Bytes>>());

        assert!(is_send::<PermitBodyImage<Bytes>>());
        assert!(is_sync::<PermitBodyImage<Bytes>>());
    }
}
