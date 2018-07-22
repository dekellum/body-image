#[cfg(unix)]
extern crate libc;

use std::io;
use std::ops::Deref;
use std::sync::Arc;

use bytes::Buf;
use memmap::Mmap;

use ::mem_util;

/// New-type for zero-copy `Buf` trait implementation of `Mmap`
#[derive(Debug)]
pub struct MemMapBuf {
    mm: Arc<Mmap>,
    pos: usize,
}

impl MemMapBuf {
    pub(crate) fn new(mmap: Arc<Mmap>) -> MemMapBuf {
        MemMapBuf { mm: mmap, pos: 0 }
    }

    /// Advise the \*nix OS that we will be sequentially accessing the memory
    /// map region, and that agressive read-ahead is warranted.
    pub(crate) fn advise_sequential(&self) -> Result<(), io::Error> {
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

impl Deref for MemMapBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.mm[self.pos..]
    }
}

impl AsRef<[u8]> for MemMapBuf {
    fn as_ref(&self) -> &[u8] {
        &self.mm[self.pos..]
    }
}
