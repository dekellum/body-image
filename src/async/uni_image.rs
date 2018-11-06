use std::cmp;
use std::io;
use std::io::{Cursor, Read};
use std::ops::Deref;
use std::vec::IntoIter;

use ::http;
use olio::fs::rc::ReadSlice;
use bytes::{Buf, BufMut, Bytes, BytesMut, IntoBuf};
use failure::Error as Flare;

use async::hyper;
use async::tokio_threadpool;
use async::futures::{Async, Poll, Stream};
use async::{MemMapBuf, RequestRecord, RequestRecorder};
use ::{BodyImage, ExplodedImage, Prolog, Tunables};

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

#[derive(Debug)]
enum UniBodyState {
    Ram(IntoIter<Bytes>),
    File { rs: ReadSlice, bsize: u64 },
    MemMap(Option<MemMapBuf>),
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
                "UniBodyImage needs `blocking`, \
                 backup threads of Tokio threadpool"
            ))
        }
    }
}

impl Stream for UniBodyImage {
    type Item = UniBodyBuf;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<UniBodyBuf>, io::Error> {
        match self.state {
            UniBodyState::Ram(ref mut iter) => {
                let n = iter.next();
                if let Some(b) = n {
                    self.consumed += b.len() as u64;
                    Ok(Async::Ready(Some(UniBodyBuf {
                        buf: BufState::Bytes(b.into_buf())
                    })))
                } else {
                    Ok(Async::Ready(None))
                }
            }
            UniBodyState::File { ref mut rs, bsize } => {
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
                            let b = buf.freeze().into_buf();
                            debug!("read chunk (blocking, len: {})", len);
                            Ok(Some(UniBodyBuf { buf: BufState::Bytes(b) }))
                        }
                        Err(e) => Err(e)
                    }
                });
                if let Ok(Async::Ready(Some(ref b))) = res {
                    self.consumed += b.len() as u64;
                }
                res
            }
            UniBodyState::MemMap(ref mut ob) => {
                let d = ob.take();
                if let Some(mb) = d {
                    let res = unblock(|| {
                        mb.advise_sequential()?;
                        let _b = mb.bytes()[0];
                        debug!("prepared MemMap (blocking, len: {})", mb.len());
                        Ok(Some(UniBodyBuf { buf: BufState::MemMap(mb) }))
                    });
                    if let Ok(Async::Ready(Some(ref b))) = res {
                        self.consumed += b.len() as u64;
                    }
                    res
                } else {
                    Ok(Async::Ready(None))
                }
            }
        }
    }
}

impl hyper::body::Payload for UniBodyImage {
    type Data = UniBodyBuf;
    type Error = io::Error;

    fn poll_data(&mut self) -> Poll<Option<UniBodyBuf>, io::Error> {
        self.poll()
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.len)
    }

    fn is_end_stream(&self) -> bool {
        self.consumed >= self.len
    }
}

impl RequestRecorder<UniBodyImage> for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord<UniBodyImage>, Flare> {
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
        -> Result<RequestRecord<UniBodyImage>, Flare>
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
        -> Result<RequestRecord<UniBodyImage>, Flare>
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
