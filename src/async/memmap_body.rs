#[cfg(unix)]
extern crate libc;

use std::io;
use std::sync::Arc;

use ::http;
use bytes::Buf;
use memmap::Mmap;
use failure::Error as Flare;

use async::hyper;
use async::tokio_threadpool;
use async::futures::{Async, Poll, Stream};
use async::{RequestRecord, RequestRecordableEmpty, RequestRecordableImage};
use ::{BodyImage, ExplodedImage, Prolog, Tunables};
use ::mem_util;

/// Experimental, specialized adaptor for `BodyImage` in `MemMap` state,
/// implementating `futures::Stream` and `hyper::body::Payload` traits
/// with zero-copy.
///
/// ## Implementation Notes
///
/// This uses `tokio_threadpool::blocking` to request becoming a backup thread
/// before:
///
/// 1. On a \*nix OS, if applicable, advise of imminent *sequential* access to
/// the memory region of the map (see excerpt below).
///
/// 2. Referencing the first byte of the memory mapped region.
///
/// Beyond this initial blocking annotation, its presumed a race between OS
/// read-ahead and storage, Tokio and TCP or other streams; not unlike what
/// would happen in a virtual memory swapping situation with many large bodies
/// exceeding physical RAM (author waves hands).
///
/// Upon `Drop` of the `MemMapBuf` (`Payload::Data` unit type), if on a \*nix
/// OS, we conclude by advising of *normal* access (see below) to the memory
/// mapped region, as if *sequential* access had never been requested. This
/// behavior is logically symetric, but doesn't account for the fact that the
/// `BodyImage` may have been cloned and the memory map remains alive for
/// other, potentially concurrent use. It's likely not fatal, if the same
/// pattern is used for all `MemMap` handle accesses, and given the advisory
/// nature, but its definately racy.
///
/// ## Excerpt from GNU/Linux POSIX_MADVISE(3)
///
/// > ### POSIX_MADV_SEQUENTIAL
/// >
/// > The application expects to access the specified address range
/// > sequentially, running from lower addresses to higher addresses. Hence,
/// > pages in this region can be aggressively read ahead, and may be freed
/// > soon after they are accessed.
/// >
/// > *\[…\]*
/// >
/// > ### POSIX_MADV_NORMAL
/// >
/// > The application has no special advice regarding its memory usage
/// > patterns for the specified address range.  This is the default behavior.
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
    /// Advise the \*nix OS that we will be sequentially accessing the memory
    /// map region, and that agressive read-ahead is warranted.
    fn advise_sequential(&self) -> Result<(), io::Error> {
        mem_util::advise(
            self.mm.as_ref(),
            &[mem_util::MemoryAccess::Sequential]
        )
    }
}

impl Drop for MemMapBuf {
    fn drop(&mut self) {
        mem_util::advise(
            self.mm.as_ref(),
            &[mem_util::MemoryAccess::Normal]
        ).ok();
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
    /// beforehand to preserve an owned copy. Returns as per `empty()` if the
    /// the body is empty. Otherwise panics if the supplied
    /// `BodyImage` is not in `MemMap` state (e.g. `BodyImage::is_mem_map`
    /// returns `true`.)
    pub fn new(body: BodyImage) -> AsyncMemMapBody {
        if body.is_empty() { return Self::empty() };
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

    /// Construct optimal (no allocation) empty body representation.
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

impl Stream for AsyncMemMapBody {
    type Item = MemMapBuf;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<MemMapBuf>, io::Error> {
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
}

impl hyper::body::Payload for AsyncMemMapBody {
    type Data = MemMapBuf;
    type Error = io::Error;

    fn poll_data(&mut self) -> Poll<Option<MemMapBuf>, io::Error> {
        self.poll()
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
