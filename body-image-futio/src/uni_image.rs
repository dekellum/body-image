use std::cmp;
use std::fmt;
use std::future::Future;
use std::io::Read;
use std::io;
use std::mem;
use std::ops::Deref;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use blocking_permit::{
    blocking_permit_future, BlockingPermitFuture,
    dispatch_rx, Dispatched, is_dispatch_pool_registered,
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_core::stream::Stream;
use olio::fs::rc::ReadSlice;
use tao_log::{debug, info, warn};

use body_image::{BodyImage, ExplodedImage, Prolog, };

use crate::{
    FutioTunables, RequestRecord, RequestRecorder, StreamWrapper,
};

#[cfg(feature = "mmap")] use crate::MemMapBuf;

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
pub struct UniBodyImage {
    state: UniBodyState,
    delegate: Delegate,
    len: u64,
    consumed: u64,
    tune: FutioTunables,
}

#[derive(Debug)]
enum Delegate {
    Dispatch(Dispatched<(
        Result<Option<UniBodyBuf>, io::Error>,
        UniBodyState
    )>),
    Permit(BlockingPermitFuture<'static>),
    None
}

impl UniBodyImage {
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` and `FutioTunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    pub fn new(body: BodyImage, tune: FutioTunables) -> UniBodyImage {
        let len = body.len();
        match body.explode() {
            ExplodedImage::Ram(v) => {
                UniBodyImage {
                    state: UniBodyState::Ram(v.into_iter()),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                    tune,
                }
            }
            ExplodedImage::FsRead(rs) => {
                UniBodyImage {
                    state: UniBodyState::File(rs),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                    tune,
                }
            }
            #[cfg(feature = "mmap")]
            ExplodedImage::MemMap(mmap) => {
                UniBodyImage {
                    state: UniBodyState::MemMap(Some(MemMapBuf::new(mmap))),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                    tune,
                }
            }
        }
    }
}

impl StreamWrapper for UniBodyImage {
    fn new(body: BodyImage, tune: FutioTunables) -> UniBodyImage {
        UniBodyImage::new(body, tune)
    }
}

/// Provides zero-copy read access to both `Bytes` and `Mmap` memory
/// regions. Implements `bytes::Buf` (*mmap* feature only).
#[derive(Debug)]
pub struct UniBodyBuf {
    buf: BufState
}

#[derive(Debug)]
enum BufState {
    Bytes(Bytes),
    #[cfg(feature = "mmap")]
    MemMap(MemMapBuf),
}

impl UniBodyBuf {
    pub(crate) fn empty() -> UniBodyBuf {
        UniBodyBuf::from_bytes(Bytes::new())
    }

    pub(crate) fn from_bytes(b: Bytes) -> UniBodyBuf {
        UniBodyBuf { buf: BufState::Bytes(b) }
    }

    #[cfg(feature = "mmap")]
    pub(crate) fn from_mmap(mb: MemMapBuf) -> UniBodyBuf {
        UniBodyBuf { buf: BufState::MemMap(mb) }
    }
}

impl Buf for UniBodyBuf {
    fn remaining(&self) -> usize {
        match self.buf {
            BufState::Bytes(ref b)  => b.remaining(),
            #[cfg(feature = "mmap")]
            BufState::MemMap(ref b) => b.remaining(),
        }
    }

    fn bytes(&self) -> &[u8] {
        match self.buf {
            BufState::Bytes(ref b)  => b.bytes(),
            #[cfg(feature = "mmap")]
            BufState::MemMap(ref b) => b.bytes(),
        }
    }

    fn advance(&mut self, count: usize) {
        match self.buf {
            BufState::Bytes(ref mut b)  => b.advance(count),
            #[cfg(feature = "mmap")]
            BufState::MemMap(ref mut b) => b.advance(count),
        }
    }
}

impl Deref for UniBodyBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes()
    }
}

impl AsRef<[u8]> for UniBodyBuf {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

impl Into<Bytes> for UniBodyBuf {
    fn into(self) -> Bytes {
        match self.buf {
            BufState::Bytes(b) => b,
            #[cfg(feature = "mmap")]
            BufState::MemMap(_mb) => {
                panic!("FIXME: Don't do this so cheaply!");
                // Bytes::copy_from_slice(&mb[..]),
            }
        }
    }
}

impl From<Bytes> for UniBodyBuf {
    fn from(b: Bytes) -> UniBodyBuf {
        UniBodyBuf { buf: BufState::Bytes(b) }
    }
}

enum UniBodyState {
    Ram(IntoIter<Bytes>),
    File(ReadSlice),
    #[cfg(feature = "mmap")]
    MemMap(Option<MemMapBuf>),
    Delegated,
}

impl fmt::Debug for UniBodyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            UniBodyState::Ram(_) => {
                // Avoids showing all buffers as u8 lists
                write!(f, "Ram(IntoIter<Bytes>)")
            }
            UniBodyState::File(ref rs) => {
                f.debug_struct("File")
                    .field("rs", rs)
                    .finish()
            }
            #[cfg(feature = "mmap")]
            UniBodyState::MemMap(ref ob) => {
                f.debug_tuple("MemMap")
                    .field(ob)
                    .finish()
            }
            UniBodyState::Delegated => {
                f.debug_struct("Delegated")
                    .finish()
            }

        }
    }
}

impl UniBodyState {
    fn read_next(&mut self, len: usize)
        -> Result<Option<UniBodyBuf>, io::Error>
    {
        match *self {
            UniBodyState::File(ref mut rs) => {
                let mut buf = BytesMut::with_capacity(len);
                let b = unsafe {
                    &mut *(buf.bytes_mut()
                           as *mut [mem::MaybeUninit<u8>] as *mut [u8])
                };
                match rs.read(&mut b[..len]) {
                    Ok(0) => Ok(None),
                    Ok(len) => {
                        unsafe { buf.advance_mut(len); }
                        debug!("read chunk (len: {})", len);
                        Ok(Some(UniBodyBuf::from_bytes(buf.freeze())))
                    }
                    Err(e) => Err(e)
                }
            }
            #[cfg(feature = "mmap")]
            UniBodyState::MemMap(ref mut ob) => {
                if let Some(mb) = ob.take() {
                    mb.advise_sequential()?;
                    let _b = mb.bytes()[0];
                    debug!("prepared MemMap (len: {})", mb.len());
                    Ok(Some(UniBodyBuf::from_mmap(mb)))
                } else {
                    Ok(None)
                }
            }
            UniBodyState::Ram(_) => unimplemented!(),
            UniBodyState::Delegated => unimplemented!(),
        }
    }
}

impl Stream for UniBodyImage {
    type Item = Result<UniBodyBuf, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        let this = unsafe { self.get_unchecked_mut() };

        // Handle any delegate futures (new permit or early exit)
        let permit = match this.delegate {
            Delegate::Dispatch(ref mut db) => {
                info!("delegate poll to Dispatched");
                let (poll_res, rstate) = match Pin::new(&mut *db).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                    Poll::Ready(Ok((Ok(None), st))) => (Poll::Ready(None), st),
                    Poll::Ready(Ok((Ok(Some(b)), st))) => (Poll::Ready(Some(Ok(b))), st),
                    Poll::Ready(Ok((Err(e), st))) => {
                        if e.kind() == io::ErrorKind::Interrupted {
                            warn!("UniBodyImage: write interrupted");
                            cx.waker().wake_by_ref(); // Ensure re-poll
                            (Poll::Pending, st)
                        } else {
                            (Poll::Ready(Some(Err(e))), st)
                        }
                    }
                };
                this.delegate = Delegate::None;
                this.state = rstate;

                if let Poll::Ready(Some(Ok(ref b))) = poll_res {
                    this.consumed += b.len() as u64;
                }
                return poll_res;
            }
            Delegate::Permit(ref mut pf) => {
                let pf = unsafe { Pin::new_unchecked(pf) };
                match pf.poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(p)) => {
                        this.delegate = Delegate::None;
                        Some(p)
                    }
                    Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
                }
            }
            Delegate::None => None
        };

        // Ram doesn't need blocking permit (early exit)
        if let UniBodyState::Ram(ref mut iter) = this.state {
            return match iter.next() {
                Some(b) => {
                    this.consumed += b.len() as u64;
                    Poll::Ready(Some(Ok(UniBodyBuf::from_bytes(b))))
                }
                None => Poll::Ready(None),
            }
        }

        // Early exit if nothing available
        let avail = this.len - this.consumed;
        if avail == 0 {
            return Poll::Ready(None);
        }

        // Length to read is minimum of (fs) buffer size and the amount
        // available.
        let rlen = cmp::min(
            this.tune.image().buffer_size_fs() as u64,
            avail
        ) as usize;

        // Otherwise we'll need a permit or to dispatch (and exit early)
        if permit.is_none() {
            if is_dispatch_pool_registered() {
                let mut st = mem::replace(
                    &mut this.state,
                    UniBodyState::Delegated
                );
                this.delegate = Delegate::Dispatch(dispatch_rx(move || {
                    (st.read_next(rlen), st)
                }).unwrap());
            } else {
                let f = blocking_permit_future(
                    this.tune.blocking_semaphore()
                        .expect("One of DispatchPool or \
                                 blocking Semaphore required!")
                );
                this.delegate = Delegate::Permit(f);
            }
            // recurse once (needed for correct waking)
            let s = unsafe { Pin::new_unchecked(this) };
            return s.poll_next(cx);
        }

        // Use permit to run the blocking operation on thread
        let res = permit.unwrap().run(|| {
            match this.state.read_next(rlen) {
                Ok(Some(b)) => Poll::Ready(Some(Ok(b))),
                Ok(None) => Poll::Ready(None),
                Err(e) => {
                    if e.kind() == io::ErrorKind::Interrupted {
                        warn!("UniBodyImage: write interrupted");
                        cx.waker().wake_by_ref(); //ensure re-poll
                        Poll::Pending
                    } else {
                        Poll::Ready(Some(Err(e)))
                    }
                }
            }
        });

        if let Poll::Ready(Some(Ok(ref b))) = res {
            this.consumed += b.len() as u64;
        }
        res
    }
}

impl http_body::Body for UniBodyImage {
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

impl RequestRecorder<UniBodyImage> for http::request::Builder {
    fn record(self) -> Result<RequestRecord<UniBodyImage>, http::Error> {
        let request = {
            let body = BodyImage::empty();
            // Tunables are unused for empty body, so default is sufficient.
            let tune = FutioTunables::default();
            self.body(UniBodyImage::new(body, tune))?
        };
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = BodyImage::empty();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body }
        })
    }

    fn record_body<BB>(self, body: BB)
        -> Result<RequestRecord<UniBodyImage>, http::Error>
        where BB: Into<Bytes>
    {
        let buf: Bytes = body.into();
        let req_body = if buf.is_empty() {
            BodyImage::empty()
        } else {
            BodyImage::from_slice(buf)
        };
        // Tunables are unused for Ram based body, so default is sufficient.
        let tune = FutioTunables::default();
        let request = self.body(UniBodyImage::new(req_body.clone(), tune))?;

        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body_image(self, body: BodyImage, tune: FutioTunables)
        -> Result<RequestRecord<UniBodyImage>, http::Error>
    {
        let request = self.body(UniBodyImage::new(body.clone(), tune))?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body: body } })
    }
}
