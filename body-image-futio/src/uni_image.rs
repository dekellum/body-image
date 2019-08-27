use std::cmp;
use std::fmt;
use std::io::{Cursor, Read};
use std::io;
use std::ops::Deref;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use bytes::{Buf, BufMut, Bytes, BytesMut, IntoBuf};
use futures::stream::Stream;
use http;
use hyper;
use olio::fs::rc::ReadSlice;
use tao_log::{debug, warn};
use tokio_executor::threadpool as tokio_threadpool;

use body_image::{BodyImage, ExplodedImage, Prolog, Tunables};

use crate::{MemMapBuf, RequestRecord, RequestRecorder, StreamWrapper};

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
/// consuming iterator.  This implementation uses `tokio_threadpool::blocking`
/// to request becoming a backup thread for blocking reads from `FsRead` state
/// and when dereferencing from `MemMap` state.
#[derive(Debug)]
pub struct UniBodyImage {
    state: UniBodyState,
    len: u64,
    consumed: u64,
}

impl StreamWrapper for UniBodyImage {
    fn new(body: BodyImage, tune: &Tunables) -> UniBodyImage {
        let len = body.len();
        match body.explode() {
            ExplodedImage::Ram(v) => {
                UniBodyImage {
                    state: UniBodyState::Ram(v.into_iter()),
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
                    len,
                    consumed: 0,
                }
            }
            ExplodedImage::MemMap(mmap) => {
                UniBodyImage {
                    state: UniBodyState::MemMap(Some(MemMapBuf::new(mmap))),
                    len,
                    consumed: 0,
                }
            }
        }
    }
}

/// Provides zero-copy read access to both `Bytes` and `Mmap` memory
/// regions. Implements `bytes::Buf` (*mmap* feature only).
pub struct UniBodyBuf {
    buf: BufState
}

enum BufState {
    Bytes(Cursor<Bytes>),
    MemMap(MemMapBuf),
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
        }
    }
}

fn unblock<F, T>(_cx: &mut Context<'_>, f: F)
    -> Poll<Option<Result<T, io::Error>>>
    where F: FnOnce() -> io::Result<Option<T>>
{
    match tokio_threadpool::blocking(f) {
        Poll::Pending => {
            warn!("UniBodyImage: no blocking backup thread available");
            Poll::Pending
        }
        Poll::Ready(Ok(Ok(Some(v)))) => Poll::Ready(Some(Ok(v))),
        Poll::Ready(Ok(Ok(None))) => Poll::Ready(None),
        Poll::Ready(Ok(Err(e))) => {
            if e.kind() == io::ErrorKind::Interrupted {
                warn!("UniBodyImage: write interrupted");
                Poll::Pending
            } else {
                Poll::Ready(Some(Err(e)))
            }
        }
        Poll::Ready(Err(_)) => {
            Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::Other,
                "UniBodyImage needs `blocking`, \
                 backup threads of Tokio threadpool"
            ))))
        }
    }
}

impl Stream for UniBodyImage {
    type Item = Result<UniBodyBuf, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Self::Item>>
    {
        let this = self.get_mut();

        match this.state {
            UniBodyState::Ram(ref mut iter) => {
                let n = iter.next();
                if let Some(b) = n {
                    this.consumed += b.len() as u64;
                    Poll::Ready(Some(Ok(
                        UniBodyBuf { buf: BufState::Bytes(b.into_buf()) }
                    )))
                } else {
                    Poll::Ready(None)
                }
            }
            UniBodyState::File { ref mut rs, bsize } => {
                let avail = this.len - this.consumed;
                if avail == 0 {
                    return Poll::Ready(None);
                }
                let res = unblock(cx, || {
                    let bs = cmp::min(bsize, avail) as usize;
                    let mut buf = BytesMut::with_capacity(bs);
                    match rs.read(unsafe { &mut buf.bytes_mut()[..bs] }) {
                        Ok(0) => Ok(None),
                        Ok(len) => {
                            unsafe { buf.advance_mut(len); }
                            let b = buf.freeze().into_buf();
                            debug!("read chunk (blocking, len: {})", len);
                            Ok(Some(UniBodyBuf { buf: BufState::Bytes(b) }))
                        }
                        Err(e) => Err(e)
                    }
                });
                if let Poll::Ready(Some(Ok(ref b))) = res {
                    this.consumed += b.len() as u64;
                }
                res
            }
            UniBodyState::MemMap(ref mut ob) => {
                let d = ob.take();
                if let Some(mb) = d {
                    let res = unblock(cx, || {
                        mb.advise_sequential()?;
                        let _b = mb.bytes()[0];
                        debug!("prepared MemMap (blocking, len: {})", mb.len());
                        Ok(Some(UniBodyBuf { buf: BufState::MemMap(mb) }))
                    });
                    if let Poll::Ready(Some(Ok(ref b))) = res {
                        this.consumed += b.len() as u64;
                    }
                    res
                } else {
                    Poll::Ready(None)
                }
            }
        }
    }
}

impl hyper::body::Payload for UniBodyImage {
    type Data = UniBodyBuf;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        self.poll_next(cx)
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.len)
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
