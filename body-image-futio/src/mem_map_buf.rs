use std::io;
use std::ops::Deref;

use bytes::Buf;
use memmap::Mmap;
use olio::mem::{MemAdvice, MemHandle};
#[cfg(unix)] use tao_log::debug;

/// New-type for zero-copy `Buf` trait implementation of `MemHandle<Mmap>`
#[derive(Debug)]
pub struct MemMapBuf {
    mm: MemHandle<Mmap>,
    pos: usize,
    len: usize,
}

impl MemMapBuf {
    pub(crate) fn new(mmap: MemHandle<Mmap>) -> MemMapBuf {
        let len = mmap.len();
        MemMapBuf { mm: mmap, pos: 0, len }
    }

    /// Split into two at a given index, returning the leading segment.
    ///
    /// The leading [0, rel) bytes segment is returned, and self will
    /// subsequently contain [rel, len).
    ///
    /// # Panics
    ///
    /// Panics if rel is out of bounds or if either segment would be zero
    /// length.
    pub(crate) fn split_to(&mut self, rel: usize) -> MemMapBuf {
        assert!(rel > 0);
        let abs = self.pos + rel;
        assert!(abs < self.len);

        let ret = MemMapBuf { mm: self.mm.clone(), pos: self.pos, len: abs };
        self.pos = abs;
        ret
    }

    /// Advise that we will be sequentially accessing the memory map region,
    /// and that aggressive read-ahead is warranted.
    pub(crate) fn advise_sequential(&self) -> Result<(), io::Error> {
        let _new_advice = self.mm.advise(MemAdvice::Sequential)?;
        #[cfg(unix)] {
            debug!("MemMapBuf advise Sequential, obtained {:?}", _new_advice);
        }
        Ok(())
    }
}

impl Buf for MemMapBuf {
    fn remaining(&self) -> usize {
        self.len - self.pos
    }

    fn bytes(&self) -> &[u8] {
        &self.mm[self.pos..self.len]
    }

    fn advance(&mut self, rel: usize) {
        assert!(rel <= self.remaining(), "MemMapBuf::advance past end");
        self.pos += rel;
    }
}

impl Deref for MemMapBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.mm[self.pos..self.len]
    }
}

impl AsRef<[u8]> for MemMapBuf {
    fn as_ref(&self) -> &[u8] {
        &self.mm[self.pos..self.len]
    }
}
