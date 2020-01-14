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
}

impl MemMapBuf {
    pub(crate) fn new(mmap: MemHandle<Mmap>) -> MemMapBuf {
        MemMapBuf { mm: mmap, pos: 0 }
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
