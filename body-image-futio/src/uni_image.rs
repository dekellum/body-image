use std::cmp;
use std::fmt;
use std::future::Future;
use std::io::{Cursor, Read};
use std::io;
use std::mem;
use std::ops::Deref;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use blocking_permit::{
    blocking_permit_future, BlockingPermitFuture,
    dispatch_rx, DispatchBlocking,
    IsReactorThread,
};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::stream::Stream;
use http;
use olio::fs::rc::ReadSlice;
use tao_log::{debug, info, warn};

use body_image::{BodyImage, ExplodedImage, Prolog, Tunables};

use crate::{
    BLOCKING_SET,
    MemMapBuf, RequestRecord, RequestRecorder, StreamWrapper
};

/// Adaptor for `BodyImage` implementing the `futures::Stream` and
/// `hyper::body::Payload` traits, using the custom
/// [`UniBodyBuf`](struct.UniBodyBuf.html) item buffer type (instead of
/// `Bytes`) for zero-copy `MemMap` support (*mmap* feature only).
///
/// The `Payload` trait (plus `Send`) makes this usable with hyper as the `B`
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
}

#[derive(Debug)]
enum Delegate {
    Dispatch(DispatchBlocking<(
        Result<Option<UniBodyBuf>, io::Error>,
        UniBodyState
    )>),
    Permit(BlockingPermitFuture<'static>),
    None
}

impl UniBodyImage {
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    pub fn new(body: BodyImage, tune: &Tunables) -> UniBodyImage {
        let len = body.len();
        match body.explode() {
            ExplodedImage::Ram(v) => {
                UniBodyImage {
                    state: UniBodyState::Ram(v.into_iter()),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                }
            }
            ExplodedImage::FsRead(rs) => {
                UniBodyImage {
                    state: UniBodyState::File {
                        rs,
                        bsize: tune.buffer_size_fs() as u64
                    },
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                }
            }
            ExplodedImage::MemMap(mmap) => {
                UniBodyImage {
                    state: UniBodyState::MemMap(Some(MemMapBuf::new(mmap))),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                }
            }
        }
    }
}

impl StreamWrapper for UniBodyImage {
    fn new(body: BodyImage, tune: &Tunables) -> UniBodyImage {
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
    Bytes(Cursor<Bytes>),
    MemMap(MemMapBuf),
}

impl UniBodyBuf {
    pub(crate) fn empty() -> UniBodyBuf {
        UniBodyBuf::from_bytes(Bytes::with_capacity(0))
    }

    fn from_bytes<B>(b: B) -> UniBodyBuf
        where B: IntoBuf<Buf=Cursor<Bytes>>
    {
        UniBodyBuf { buf: BufState::Bytes(b.into_buf()) }
    }

    fn from_mmap(mb: MemMapBuf) -> UniBodyBuf {
        UniBodyBuf { buf: BufState::MemMap(mb) }
    }
}

impl Buf for UniBodyBuf {
    fn remaining(&self) -> usize {
        match self.buf {
            BufState::Bytes(ref c)  => c.remaining(),
            BufState::MemMap(ref b) => b.remaining(),
        }
    }

    fn bytes(&self) -> &[u8] {
        match self.buf {
            BufState::Bytes(ref c)  => c.bytes(),
            BufState::MemMap(ref b) => b.bytes(),
        }
    }

    fn advance(&mut self, count: usize) {
        match self.buf {
            BufState::Bytes(ref mut c)  => c.advance(count),
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

enum UniBodyState {
    Ram(IntoIter<Bytes>),
    File { rs: ReadSlice, bsize: u64 },
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
            UniBodyState::File { ref rs, ref bsize } => {
                f.debug_struct("File")
                    .field("rs", rs)
                    .field("bsize", bsize)
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
    fn read_next(&mut self, avail: u64) -> Result<Option<UniBodyBuf>, io::Error> {
        match *self {
            UniBodyState::File { ref mut rs, bsize } => {
                let bs = cmp::min(bsize, avail) as usize;
                let mut buf = BytesMut::with_capacity(bs);
                match rs.read(unsafe { &mut buf.bytes_mut()[..bs] }) {
                    Ok(0) => Ok(None),
                    Ok(len) => {
                        unsafe { buf.advance_mut(len); }
                        debug!("read chunk (len: {})", len);
                        Ok(Some(UniBodyBuf::from_bytes(buf.freeze())))
                    }
                    Err(e) => Err(e)
                }
            }
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
        let this = self.get_mut();

        // Handle any delegate futures (new permit or early exit)
        let permit = match this.delegate {
            Delegate::Dispatch(ref mut db) => {
                info!("delegate poll to DispatchBlocking");
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
                match Pin::new(pf).poll(cx) {
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

        // Otherwise we'll need a permit or to dispatch (and exit early)
        if permit.is_none() {
            match blocking_permit_future(&BLOCKING_SET) {
                Ok(f) => {
                    this.delegate = Delegate::Permit(f);
                }
                Err(IsReactorThread) => {
                    let mut st = mem::replace(
                        &mut this.state,
                        UniBodyState::Delegated
                    );
                    this.delegate = Delegate::Dispatch(dispatch_rx(move || {
                        (st.read_next(avail), st)
                    }));
                },
            }
            // recurse once (needed for correct waking)
            return Pin::new(this).poll_next(cx);
        }

        // Use permit to run the blocking operation on thread
        let res = permit.unwrap().run_unwrap(|| {
            match this.state.read_next(avail) {
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
    fn record(&mut self) -> Result<RequestRecord<UniBodyImage>, http::Error> {
        let request = {
            let body = BodyImage::empty();
            let tune = Tunables::default();
            self.body(UniBodyImage::new(body, &tune))?
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

    fn record_body<BB>(&mut self, body: BB)
        -> Result<RequestRecord<UniBodyImage>, http::Error>
        where BB: Into<Bytes>
    {
        let buf: Bytes = body.into();
        let req_body = if buf.is_empty() {
            BodyImage::empty()
        } else {
            BodyImage::from_slice(buf)
        };
        let tune = Tunables::default();
        let request = self.body(UniBodyImage::new(req_body.clone(), &tune))?;

        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body_image(&mut self, body: BodyImage, tune: &Tunables)
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
