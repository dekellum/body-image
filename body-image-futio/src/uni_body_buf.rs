use std::ops::Deref;
use bytes::{Buf, Bytes};

#[cfg(feature = "mmap")] use crate::MemMapBuf;

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

impl From<Bytes> for UniBodyBuf {
    fn from(b: Bytes) -> Self {
        UniBodyBuf { buf: BufState::Bytes(b) }
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
