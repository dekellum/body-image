use std::ops::Deref;
use bytes::{Buf, Bytes};
use blocking_permit::Splittable;

#[cfg(feature = "mmap")]
use {
    tao_log::debug,

    crate::MemMapBuf,
};

/// Provides zero-copy read access to both `Bytes` and memory mapped regions
/// (`MemMapBuf`). Implements `bytes::Buf` (*mmap* feature only).
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

    fn chunk(&self) -> &[u8] {
        match self.buf {
            BufState::Bytes(ref b)  => b.chunk(),
            #[cfg(feature = "mmap")]
            BufState::MemMap(ref b) => b.chunk(),
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
        self.chunk()
    }
}

impl AsRef<[u8]> for UniBodyBuf {
    fn as_ref(&self) -> &[u8] {
        self.chunk()
    }
}

impl Splittable for UniBodyBuf {
    fn split_if(&mut self, max: usize) -> Option<Self> {
        if self.len() > max {
            match self.buf {
                BufState::Bytes(ref mut b) => {
                    Some(UniBodyBuf { buf: BufState::Bytes(b.split_to(max)) })
                }
                #[cfg(feature = "mmap")]
                BufState::MemMap(ref mut mb) => {
                    Some(UniBodyBuf { buf: BufState::MemMap(mb.split_to(max)) })
                }
            }
        } else {
            None
        }
    }

}

impl From<Bytes> for UniBodyBuf {
    fn from(b: Bytes) -> Self {
        UniBodyBuf { buf: BufState::Bytes(b) }
    }
}

impl From<UniBodyBuf> for Bytes {
    /// Convert from `UniBodyBuf` to Bytes. This is a costly memory copy only
    /// in the case of `MemMap` (*mmap* feature) to `Bytes`. This case is
    /// logged and should be rare using normal configuration and high-level
    /// API. For example, it could occur when a `Stream` of `UniBodyBuf` over
    /// an `MemMap` `BodyImage` is forwarded to a `Sink` configured with a
    /// larger `Tunables::max_body_ram`.  With the same maximums used to
    /// produce the original image and output sink, this should not occur, as
    /// no conversion is required when writing to `FsWrite` state.
    fn from(ubb: UniBodyBuf) -> Bytes {
        match ubb.buf {
            BufState::Bytes(b) => b,
            #[cfg(feature = "mmap")]
            BufState::MemMap(mb) => {
                debug!("copying memory map slice to Bytes (len: {})", mb.len());
                Bytes::copy_from_slice(&mb[..])
            }
        }
    }
}
