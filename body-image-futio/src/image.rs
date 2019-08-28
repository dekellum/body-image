use std::cmp;
use std::fmt;
use std::future::Future;
use std::io::{Cursor, Read};
use std::io;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use blocking_permit::{
    blocking_permit_future,
    dispatch_rx,
    BlockingPermitFuture,
    DispatchBlocking,
    IsReactorThread,
};
use bytes::{BufMut, Bytes, BytesMut, IntoBuf};
use futures::stream::Stream;
use http;
use hyper;
use olio::fs::rc::ReadSlice;
use tao_log::{debug, info, warn};

use body_image::{BodyImage, ExplodedImage, Prolog, Tunables};

use crate::{BLOCKING_SET, RequestRecord, RequestRecorder};

#[cfg(feature = "mmap")] use memmap::Mmap;
#[cfg(feature = "mmap")] use olio::mem::{MemAdvice, MemHandle};
#[cfg(feature = "mmap")] use body_image::_mem_handle_ext::MemHandleExt;

/// Trait construction of `Stream` wrapper types.
pub trait StreamWrapper: Stream {
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    fn new(body: BodyImage, tune: &Tunables) -> Self;
}

/// Adaptor for `BodyImage` implementing the `futures::Stream` and
/// `hyper::body::Payload` traits.
///
/// The `Payload` trait (plus `Send`) makes this usable with hyper as the `B`
/// body type of `http::Request<B>` (client) or `http::Response<B>`
/// (server). The `Stream` trait is sufficient for use via
/// `hyper::Body::with_stream`.
///
/// `Tunables::buffer_size_fs` is used for reading the body when in `FsRead`
/// state. `BodyImage` in `Ram` is made available with zero-copy using a
/// consuming iterator.  This implementation uses `tokio_threadpool::blocking`
/// to request becoming a backup thread for blocking reads from `FsRead` state
/// and when dereferencing from `MemMap` state (see below).
///
/// ## MemMap
///
/// While it works without complaint, it is not generally advisable to adapt a
/// `BodyImage` in `MemMap` state with this `Payload` and `Stream` type. The
/// `Bytes` part of the contract requires a owned copy of the memory-mapped
/// region of memory, which contradicts the advantage of the memory-map. The
/// cost is confirmed by the `cargo bench stream` benchmarks.
///
/// Instead use [`UniBodyImage`](struct.UniBodyImage.html) for zero-copy
/// `MemMap` support, at the cost of the adjustments required for not using
/// the default `hyper::Body` type.
///
/// None of this applies, of course, if the *mmap* feature is disabled or if
/// `BodyImage::mem_map` is never called.
#[derive(Debug)]
pub struct AsyncBodyImage {
    state: AsyncImageState,
    delegate: Delegate,
    len: u64,
    consumed: u64
}

#[derive(Debug)]
enum Delegate {
    Dispatch(DispatchBlocking<(Result<Option<Bytes>,io::Error>, AsyncImageState)>),
    Permit(BlockingPermitFuture<'static>),
    None
}

enum AsyncImageState {
    Ram(IntoIter<Bytes>),
    File { rs: ReadSlice, bsize: u64 },
    #[cfg(feature = "mmap")]
    MemMap(MemHandle<Mmap>),
    Delegated,
}

impl StreamWrapper for AsyncBodyImage {
    fn new(body: BodyImage, tune: &Tunables) -> AsyncBodyImage {
        let len = body.len();
        match body.explode() {
            ExplodedImage::Ram(v) => {
                AsyncBodyImage {
                    state: AsyncImageState::Ram(v.into_iter()),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                }
            }
            ExplodedImage::FsRead(rs) => {
                AsyncBodyImage {
                    state: AsyncImageState::File {
                        rs,
                        bsize: tune.buffer_size_fs() as u64
                    },
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                }
            }
            #[cfg(feature = "mmap")]
            ExplodedImage::MemMap(mmap) => {
                AsyncBodyImage {
                    state: AsyncImageState::MemMap(mmap),
                    delegate: Delegate::None,
                    len,
                    consumed: 0,
                }
            }
        }
    }
}

impl fmt::Debug for AsyncImageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            AsyncImageState::Ram(_) => {
                // Avoids showing all buffers as u8 lists
                write!(f, "Ram(IntoIter<Bytes>)")
            }
            AsyncImageState::File { ref rs, ref bsize } => {
                f.debug_struct("File")
                    .field("rs", rs)
                    .field("bsize", bsize)
                    .finish()
            }
            #[cfg(feature = "mmap")]
            AsyncImageState::MemMap(ref m) => {
                f.debug_tuple("MemMap")
                    .field(m)
                    .finish()
            }
            AsyncImageState::Delegated => {
                f.debug_struct("Delegated")
                    .finish()
            }
        }
    }
}

impl AsyncImageState {
    fn read_next(&mut self, avail: u64) -> Result<Option<Bytes>, io::Error> {
        match *self {
            AsyncImageState::File { ref mut rs, bsize } => {
                let bs = cmp::min(bsize, avail) as usize;
                let mut buf = BytesMut::with_capacity(bs);
                match rs.read(unsafe { &mut buf.bytes_mut()[..bs] }) {
                    Ok(0) => Ok(None),
                    Ok(len) => {
                        unsafe { buf.advance_mut(len); }
                        debug!("read chunk (len: {})", len);
                        Ok(Some(buf.freeze()))
                    }
                    Err(e) => Err(e)
                }
            }
            #[cfg(feature = "mmap")]
            AsyncImageState::MemMap(ref mmap) => {
                // This can only copy via *bytes* `copy_from_slice`.
                match mmap.tmp_advise(
                    MemAdvice::Sequential,
                    || Ok(Bytes::from(&mmap[..])))
                {
                    Ok(b) => {
                        debug!(
                            "MemMap copy to chunk (len: {})",
                            b.len());
                        Ok(Some(b))
                    }
                    Err(e) => Err(e),
                }
            }
            AsyncImageState::Ram(_) => unimplemented!(),
            AsyncImageState::Delegated => unimplemented!(),
        }
    }
}

impl hyper::body::Payload for AsyncBodyImage {
    type Data = Cursor<Bytes>;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        match self.poll_next(cx) {
            Poll::Pending              => Poll::Pending,
            Poll::Ready(Some(Ok(b)))   => Poll::Ready(Some(Ok(b.into_buf()))),
            Poll::Ready(Some(Err(e)))  => Poll::Ready(Some(Err(e))),
            Poll::Ready(None)          => Poll::Ready(None)
        }
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.len)
    }

    fn is_end_stream(&self) -> bool {
        self.consumed >= self.len
    }
}

impl Stream for AsyncBodyImage {
    type Item = Result<Bytes, io::Error>;

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
                            warn!("AsyncBodyImage: write interrupted");
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
        if let AsyncImageState::Ram(ref mut iter) = this.state {
            return match iter.next() {
                Some(b) => {
                    this.consumed += b.len() as u64;
                    Poll::Ready(Some(Ok(b)))
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
                        AsyncImageState::Delegated
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
                        warn!("AsyncBodyImage: write interrupted");
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

impl RequestRecorder<AsyncBodyImage> for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord<AsyncBodyImage>, http::Error> {
        let request = {
            let body = BodyImage::empty();
            let tune = Tunables::default();
            self.body(AsyncBodyImage::new(body, &tune))?
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
        -> Result<RequestRecord<AsyncBodyImage>, http::Error>
        where BB: Into<Bytes>
    {
        let buf: Bytes = body.into();
        let req_body = if buf.is_empty() {
            BodyImage::empty()
        } else {
            BodyImage::from_slice(buf)
        };
        let tune = Tunables::default();
        let request = self.body(AsyncBodyImage::new(req_body.clone(), &tune))?;

        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body_image(&mut self, body: BodyImage, tune: &Tunables)
        -> Result<RequestRecord<AsyncBodyImage>, http::Error>
    {
        let request = self.body(AsyncBodyImage::new(body.clone(), tune))?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body: body } })
    }
}
