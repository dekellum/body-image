use std::cmp;
use std::fmt;
use std::io;
use std::io::{Cursor, Read};

use std::vec::IntoIter;

use http;
use olio::fs::rc::ReadSlice;
use bytes::{BufMut, Bytes, BytesMut, IntoBuf};
use tao_log::debug;

use body_image::{BodyImage, ExplodedImage, Prolog, Tunables};

use hyper;
use tokio_threadpool;
use futures::{Async, Poll, Stream};

#[cfg(feature = "futures_03")] use {
    std::pin::Pin,
    std::task::{Context, Poll as Poll03},
    futures03::stream::Stream as Stream03,
    tao_log::warn,
};

use crate::{RequestRecord, RequestRecorder};

#[cfg(feature = "mmap")] use memmap::Mmap;
#[cfg(feature = "mmap")] use olio::mem::{MemAdvice, MemHandle};
#[cfg(feature = "mmap")] use body_image::_mem_handle_ext::MemHandleExt;

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
    len: u64,
    consumed: u64,
}

impl AsyncBodyImage {
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    pub fn new(body: BodyImage, tune: &Tunables) -> AsyncBodyImage {
        let len = body.len();
        match body.explode() {
            ExplodedImage::Ram(v) => {
                AsyncBodyImage {
                    state: AsyncImageState::Ram(v.into_iter()),
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
                    len,
                    consumed: 0,
                }
            }
            #[cfg(feature = "mmap")]
            ExplodedImage::MemMap(mmap) => {
                AsyncBodyImage {
                    state: AsyncImageState::MemMap(mmap),
                    len,
                    consumed: 0,
                }
            }
        }
    }
}

enum AsyncImageState {
    Ram(IntoIter<Bytes>),
    File { rs: ReadSlice, bsize: u64 },
    #[cfg(feature = "mmap")]
    MemMap(MemHandle<Mmap>),
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
        }
    }
}

fn unblock<F, T>(f: F) -> Poll<T, io::Error>
    where F: FnOnce() -> io::Result<T>
{
    match tokio_threadpool::blocking(f) {
        Ok(Async::Ready(Ok(v))) => Ok(v.into()),
        Ok(Async::Ready(Err(e))) => {
            if e.kind() == io::ErrorKind::Interrupted {
                Ok(Async::NotReady)
            } else {
                Err(e)
            }
        }
        Ok(Async::NotReady) => Ok(Async::NotReady),
        Err(_) => {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "AsyncBodyImage needs `blocking`, \
                 backup threads of Tokio threadpool"
            ))
        }
    }
}

impl Stream for AsyncBodyImage {
    type Item = Bytes;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Bytes>, io::Error> {
        match self.state {
            AsyncImageState::Ram(ref mut iter) => {
                let n = iter.next();
                if let Some(ref b) = n {
                    self.consumed += b.len() as u64;
                }
                Ok(Async::Ready(n))
            }
            AsyncImageState::File { ref mut rs, bsize } => {
                let avail = self.len - self.consumed;
                if avail == 0 {
                    return Ok(Async::Ready(None));
                }
                let res = unblock(|| {
                    let bs = cmp::min(bsize, avail) as usize;
                    let mut buf = BytesMut::with_capacity(bs);
                    match rs.read(unsafe { &mut buf.bytes_mut()[..bs] }) {
                        Ok(0) => Ok(None),
                        Ok(len) => {
                            unsafe { buf.advance_mut(len); }
                            debug!("read chunk (blocking, len: {})", len);
                            Ok(Some(buf.freeze()))
                        }
                        Err(e) => Err(e)
                    }
                });
                if let Ok(Async::Ready(Some(ref b))) = res {
                    self.consumed += b.len() as u64;
                }
                res
            }
            #[cfg(feature = "mmap")]
            AsyncImageState::MemMap(ref mmap) => {
                let avail = self.len - self.consumed;
                if avail == 0 {
                    return Ok(Async::Ready(None));
                }
                let res = unblock(|| {
                    // This performs a copy via *bytes* crate
                    // `copy_from_slice`. There is no apparent way to achieve
                    // a 'static lifetime for `Bytes::from_static`, for
                    // example. The silver lining is that the `blocking`
                    // contract is guarunteed fullfilled here, unless of
                    // course swap is enabled and the copy is so large as to
                    // cause it to be swapped out before it is written!
                    let b = mmap.tmp_advise(
                        MemAdvice::Sequential, || -> Result<_, io::Error> {
                            Ok(Bytes::from(&mmap[..]))
                        }
                    )?;
                    debug!("MemMap copy to chunk (blocking, len: {})", b.len());
                    Ok(Some(b))
                });
                if let Ok(Async::Ready(Some(ref b))) = res {
                    self.consumed += b.len() as u64;
                }
                res
            }
        }
    }
}

impl hyper::body::Payload for AsyncBodyImage {
    type Data = Cursor<Bytes>;
    type Error = io::Error;

    fn poll_data(&mut self) -> Poll<Option<Self::Data>, io::Error> {
        match self.poll() {
            Ok(Async::Ready(Some(b))) => Ok(Async::Ready(Some(b.into_buf()))),
            Ok(Async::Ready(None))    => Ok(Async::Ready(None)),
            Ok(Async::NotReady)       => Ok(Async::NotReady),
            Err(e)                    => Err(e)
        }
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.len)
    }

    fn is_end_stream(&self) -> bool {
        self.consumed >= self.len
    }
}

#[cfg(feature = "futures_03")]
fn unblock_03<F, T>(cx: &mut Context<'_>, f: F)
    -> Poll03<Option<Result<T, io::Error>>>
    where F: FnOnce() -> io::Result<Option<T>>
{
    match tokio_threadpool::blocking(f) {
        Ok(Async::Ready(Ok(Some(v)))) => Poll03::Ready(Some(Ok(v))),
        Ok(Async::Ready(Ok(None))) => Poll03::Ready(None),
        Ok(Async::Ready(Err(e))) => {
            if e.kind() == io::ErrorKind::Interrupted {
                // Might be needed, until Tokio offers to wake for us, as part
                // of `tokio_threadpool::blocking`
                cx.waker().wake_by_ref();
                warn!("AsyncBodyImage (Stream03): write interrupted");
                Poll03::Pending
            } else {
                Poll03::Ready(Some(Err(e)))
            }
        }
        Ok(Async::NotReady) => {
            // Might be needed, until Tokio offers to wake for us, as part
            // of `tokio_threadpool::blocking`
            cx.waker().wake_by_ref();
            warn!("AsyncBodyImage (Stream03): no blocking backup thread available");
            Poll03::Pending
        }
        Err(_) => {
            Poll03::Ready(Some(Err(io::Error::new(
                io::ErrorKind::Other,
                "AsyncBodyImage (Stream03) needs `blocking`, \
                 backup threads of Tokio threadpool"
            ))))
        }
    }
}

#[cfg(feature = "futures_03")]
impl Stream03 for AsyncBodyImage {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll03<Option<Self::Item>>
    {
        let this = self.get_mut();

        match this.state {
            AsyncImageState::Ram(ref mut iter) => {
                let n = iter.next();
                if let Some(b) = n {
                    this.consumed += b.len() as u64;
                    Poll03::Ready(Some(Ok(b)))
                } else {
                    Poll03::Ready(None)
                }
            }
            AsyncImageState::File { ref mut rs, bsize } => {
                let avail = this.len - this.consumed;
                if avail == 0 {
                    return Poll03::Ready(None);
                }
                let res = unblock_03(cx, || {
                    let bs = cmp::min(bsize, avail) as usize;
                    let mut buf = BytesMut::with_capacity(bs);
                    match rs.read(unsafe { &mut buf.bytes_mut()[..bs] }) {
                        Ok(0) => Ok(None),
                        Ok(len) => {
                            unsafe { buf.advance_mut(len); }
                            debug!("read chunk (blocking, len: {})", len);
                            Ok(Some(buf.freeze()))
                        }
                        Err(e) => Err(e)
                    }
                });
                if let Poll03::Ready(Some(Ok(ref b))) = res {
                    this.consumed += b.len() as u64;
                }
                res
            }
            #[cfg(feature = "mmap")]
            AsyncImageState::MemMap(ref mmap) => {
                let avail = this.len - this.consumed;
                if avail == 0 {
                    return Poll03::Ready(None);
                }
                let res = unblock_03(cx, || {
                    // This performs a copy via *bytes* crate
                    // `copy_from_slice`. There is no apparent way to achieve
                    // a 'static lifetime for `Bytes::from_static`, for
                    // example. The silver lining is that the `blocking`
                    // contract is guarunteed fullfilled here, unless of
                    // course swap is enabled and the copy is so large as to
                    // cause it to be swapped out before it is written!
                    let b = mmap.tmp_advise(
                        MemAdvice::Sequential, || -> Result<_, io::Error> {
                            Ok(Bytes::from(&mmap[..]))
                        }
                    )?;
                    debug!("MemMap copy to chunk (blocking, len: {})", b.len());
                    Ok(Some(b))
                });
                if let Poll03::Ready(Some(Ok(ref b))) = res {
                    this.consumed += b.len() as u64;
                }
                res
            }
        }
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
