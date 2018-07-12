//! Platform independent random access memory utilities

use ::std;
use std::fmt;
use std::io;
use std::ops::Deref;

#[cfg(unix)]
use libc;

/// Possible error with `libc::(posix_)madvise()`, or any other platform
/// equivalent, to be wrapped in an `io::Error(Other)`.
#[derive(Debug)]
#[allow(dead_code)]
pub struct MadviseError {
    ecode: i32,
}

impl From<MadviseError> for io::Error {
    fn from(me: MadviseError) -> io::Error {
        io::Error::new(io::ErrorKind::Other, me)
    }
}

impl fmt::Display for MadviseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "libc::posix_madvise error return code {}", self.ecode)
    }
}

impl std::error::Error for MadviseError {
    fn description(&self) -> &str { "MadviseError" }
    fn cause(&self) -> Option<&std::error::Error> { None }
}

/// Memory Access plans, cross platform abstraction.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)]
pub enum MemoryAccess {
    Normal,
    Sequential,
    Random,
    Need,
    NoNeed,
}

/// Advise the \*nix OS about our RAM access plans.
#[cfg(unix)]
pub fn advise<T>(mem: &T, advice: &[MemoryAccess])
    -> Result<(), io::Error>
    where T: Deref<Target=[u8]>
{
    let flags: libc::c_int = advice.iter().fold(0, |m, ma| {
        m | (match *ma {
            MemoryAccess::Normal     => libc::POSIX_MADV_NORMAL,
            MemoryAccess::Sequential => libc::POSIX_MADV_SEQUENTIAL,
            MemoryAccess::Random     => libc::POSIX_MADV_RANDOM,
            MemoryAccess::Need       => libc::POSIX_MADV_WILLNEED,
            MemoryAccess::NoNeed     => libc::POSIX_MADV_DONTNEED,
        })
    });

    let ptr = &(mem[0]) as *const u8 as *mut libc::c_void;
    let res = unsafe { libc::posix_madvise(ptr, mem.len(), flags) };
    debug!("posix_madvise({:0x?}, {}, {:?}) -> {}",
           ptr, mem.len(), advice, res);

    if res == 0 {
        Ok(())
    } else {
        let me = MadviseError { ecode: res };
        Err(me.into())
    }
}

/// RAM access advice, no-op for non-\*nix OS
#[cfg(not(unix))]
pub fn advise<T>(_mem: &T, _advice: &[MemoryAccess])
    -> Result<(), io::Error>
    where T: Deref<Target=[u8]>
{
    Ok(())
}
