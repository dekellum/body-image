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

/// Memory access pattern advice, coresponding roughly to POSIX.1-2001, but
/// intending to be a workable cross platform abstraction. In particular, the
/// values do not necessarily correspond to any libc or other lib
/// constants, and are arranged in ascending order of minimal to maximum
/// *priority* in the presence of concurrent interest in the same region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
#[repr(u16)]
pub enum MemoryAccess {
    NoNeed     =  0,
    Normal     = 10,
    Need       = 20,
    Random     = 30,
    Sequential = 40,
}

/// Advise the \*nix OS about our RAM access plans.
#[cfg(unix)]
pub fn advise<T>(mem: &T, advice: &[MemoryAccess])
    -> Result<(), io::Error>
    where T: Deref<Target=[u8]>
{
    // bit-wise OR, so order independent
    let flags: libc::c_int = advice.iter().fold(0, |m, ma| {
        m | (match *ma {
            MemoryAccess::NoNeed       => libc::POSIX_MADV_DONTNEED,
            MemoryAccess::Normal       => libc::POSIX_MADV_NORMAL,
            MemoryAccess::Need         => libc::POSIX_MADV_WILLNEED,
            MemoryAccess::Random       => libc::POSIX_MADV_RANDOM,
            MemoryAccess::Sequential   => libc::POSIX_MADV_SEQUENTIAL,
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

/// RAM access advice, currently a no-op for non-\*nix OS
#[cfg(not(unix))]
pub fn advise<T>(_mem: &T, _advice: &[MemoryAccess])
    -> Result<(), io::Error>
    where T: Deref<Target=[u8]>
{
    Ok(())
}
