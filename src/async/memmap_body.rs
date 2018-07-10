extern crate libc;

use std::io;
use std::sync::Arc;

use ::http;
use bytes::Buf;
use memmap::Mmap;
use failure::Error as Flare;

use async::hyper;
use async::tokio_threadpool;
use async::futures::{Async, Poll};
use async::{RequestRecord, RequestRecordableEmpty, RequestRecordableImage};
use ::{BodyImage, ExplodedImage, Prolog, Tunables};

/// Experimental, specialized adaptor for `BodyImage` in `MemMap` state,
/// implementating the `hyper::body::Payload` trait with zero-copy.
#[derive(Debug)]
pub struct AsyncMemMapBody {
    buf: Option<MemMapBuf>,
}

/// New-type for zero-copy `Buf` trait implementation of `Mmap`
#[derive(Debug)]
pub struct MemMapBuf {
    mm: Arc<Mmap>,
    pos: usize,
}

impl MemMapBuf {
    /// Advise the OS that we will be sequentially assessing the memory map
    /// region, and thus that agressive read-ahead is advisable.
    fn advise_sequential(&self) -> Result<(), io::Error> {
        let res = unsafe {
            libc::posix_madvise(
                &(self.mm.as_ref()[0]) as *const u8 as *mut libc::c_void,
                self.mm.len(),
                libc::POSIX_MADV_SEQUENTIAL
            )
        };
        if res == 0 {
            Ok(())
        } else {
            warn!( "libc::posix_madvise return code {}", res);
            Err(io::Error::new(
                io::ErrorKind::Other,
                "posix_madvise failed"
            ))
        }
    }
}

impl Buf for MemMapBuf {
    fn remaining(&self) -> usize {
        self.mm.len() - self.pos
    }

    fn bytes(&self) -> &[u8] {
        &self.mm[self.pos..]
    }

    fn advance(&mut self, count: usize) {
        assert!(count <= self.remaining(), "MemMapBuf::advance past end");
        self.pos += count;
    }
}

impl AsyncMemMapBody {
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.  This asserts-for and will
    /// panic if the supplied `BodyImage` is not in `MemMap` state
    /// (e.g. `BodyImage::is_mem_map` returns `true`.)
    pub fn new(body: BodyImage) -> AsyncMemMapBody {
        assert!(body.is_mem_map(), "Body not MemMap");
        match body.explode() {
            ExplodedImage::MemMap(mmap) => {
                AsyncMemMapBody {
                    buf: Some(MemMapBuf {
                        mm: mmap,
                        pos: 0
                    })
                }
            },
            _ => unreachable!()
        }
    }

    pub fn empty() -> AsyncMemMapBody {
        AsyncMemMapBody { buf: None }
    }
}

macro_rules! unblock {
    (|| $b:block) => (match tokio_threadpool::blocking(|| $b) {
        Ok(Async::Ready(Ok(_))) => (),
        Ok(Async::Ready(Err(e))) => return Err(e),
        Ok(Async::NotReady) => {
            debug!("No blocking backup thread available -> NotReady");
            return Ok(Async::NotReady);
        }
        Err(_e) => {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "AsyncMemMapBody needs `blocking`, \
                 backup threads of Tokio threadpool"
            ));
        }

    })
}

impl hyper::body::Payload for AsyncMemMapBody {
    type Data = MemMapBuf;
    type Error = io::Error;

    fn poll_data(&mut self) -> Poll<Option<Self::Data>, io::Error> {
        let d = self.buf.take();
        if let Some(ref mb) = d {
            unblock!( || {
                mb.advise_sequential()?;
                let _b = mb.bytes()[0];
                debug!("read MemMapBuf (blocking, len: {})", mb.remaining());
                Ok(())
            })
        }
        Ok(Async::Ready(d))
    }

    fn content_length(&self) -> Option<u64> {
        if let Some(ref b) = self.buf {
            Some(b.remaining() as u64)
        } else {
            None
        }
    }

    fn is_end_stream(&self) -> bool {
        self.buf.is_none()
    }
}

impl RequestRecordableEmpty<AsyncMemMapBody> for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord<AsyncMemMapBody>, Flare> {
        let request = self.body(AsyncMemMapBody::empty())?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = BodyImage::empty();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body }
        })
    }
}

impl RequestRecordableImage<AsyncMemMapBody> for http::request::Builder {
    fn record_body_image(&mut self, body: BodyImage, _tune: &Tunables)
        -> Result<RequestRecord<AsyncMemMapBody>, Flare>
    {
        assert!(body.is_mem_map(), "Body not MemMap");
        let request = self.body(AsyncMemMapBody::new(body.clone()))?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body: body } })
    }
}
