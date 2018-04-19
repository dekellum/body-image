//! Experimental

use std::io;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::FileExt;

#[cfg(windows)]
use std::os::windows::fs::FileExt;

/// Implements `Seek` and `Read` for a `FileExt` reference (platform specific
/// `read_at`/`seek_read` for `File`) by maintaining an instance specific
/// position. This effectively enables independent offsets for a single `File`
/// handle.
#[derive(Debug)]
pub struct ReadPos<T: FileExt + Sync + Send>
{
    pos: u64,
    size: u64,
    file: Arc<T>,
}

// Manual implementation of clone required, apparently due to:
// https://github.com/rust-lang/rust/issues/26925
impl<T: FileExt + Sync + Send> Clone for ReadPos<T> {
    /// Return a new instance of self with position 0 (throws away
    /// any original position).
    fn clone(&self) -> ReadPos<T> {
        ReadPos { pos: 0, size: self.size, file: self.file.clone() }
    }
}

impl<T: FileExt + Sync + Send> ReadPos<T>
{
    fn new(file: T, size: u64) -> ReadPos<T> {
        ReadPos { pos: 0, size, file: Arc::new(file) }
    }

    /// Consume self and return the original T, or if there are more than one
    /// strong reference oustanding (via clone), return self.
    fn try_unwrap(self) -> Result<T, Self> {
        match Arc::try_unwrap(self.file) {
            Ok(t) => Ok(t),
            Err(arc_t) => Err(ReadPos {
                pos: self.pos,
                size: self.size,
                file: arc_t
            })
        }
    }

    #[cfg_attr(feature = "cargo-clippy", allow(collapsible_if))]
    fn seek_from(&mut self, origin: u64, offset: i64) -> io::Result<u64> {
        if offset < 0 {
            if let Some(p) = origin.checked_sub((-offset) as u64) {
                self.pos = p;
            } else {
                return Err(Error::new(
                    ErrorKind::Other,
                    "Invalid attempt to seek to a negative absolute position"
                ));
            }
        } else {
            if let Some(p) = origin.checked_add(offset as u64) {
                self.pos = p;
            } else {
                return Err(Error::new(
                    ErrorKind::Other,
                    "Attempted seek would overflow u64 position"
                ));
            }
        }
        Ok(self.pos)
    }
}

impl<T: FileExt + Sync + Send> Read for ReadPos<T>
{
    #[cfg(unix)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.file.read_at(buf, self.pos)?;
        self.pos += len as u64;
        Ok(len)
    }

    #[cfg(windows)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.file.seek_read(buf, self.pos)?;
        self.pos += len as u64;
        Ok(len)
    }
}

impl<T: FileExt + Sync + Send> Seek for ReadPos<T>
{
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match from {
            SeekFrom::Start(p) => {
                self.pos = p;
                Ok(p)
            }
            SeekFrom::End(offset) => {
                let origin = self.size;
                self.seek_from(origin, offset)
            }
            SeekFrom::Current(offset) => {
                let origin = self.pos;
                self.seek_from(origin, offset)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::thread;
    use ::tempfile::tempfile;
    use super::*;

    #[test]
    fn test_seek() {
        let mut f = tempfile().unwrap();
        f.write_all(b"1234567890").unwrap();

        let mut r1 = ReadPos::new(f, 10);
        let mut buf = [0u8; 5];

        let p = r1.seek(SeekFrom::Current(0)).unwrap();
        assert_eq!(0, p);
        let p = r1.seek(SeekFrom::Current(1)).unwrap();
        assert_eq!(1, p);
        r1.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"23456");

        let p = r1.seek(SeekFrom::End(-5)).unwrap();
        assert_eq!(5, p);
        let p = r1.seek(SeekFrom::Current(0)).unwrap();
        assert_eq!(5, p);
        r1.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"67890");
    }

    #[test]
    fn test_interleaved() {
        let mut f = tempfile().unwrap();
        f.write_all(b"1234567890").unwrap();

        let mut r1 = ReadPos::new(f, 10);

        {
            let mut buf = [0u8; 5];
            r1.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"12345");

            let mut r2 = r1.clone();
            r2.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"12345");

            r1.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"67890");

            r2.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"67890");
        }

        assert!(r1.try_unwrap().is_ok())
    }

    #[test]
    fn test_concurrent_seek_read() {
        let mut f = tempfile().unwrap();
        let rule = b"1234567890";
        f.write_all(rule).unwrap();
        let rp = ReadPos::new(f, rule.len() as u64);

        let mut threads = Vec::with_capacity(30);
        for i in 0..50 {
            let mut rpc = rp.clone();
            threads.push(thread::spawn( move || {
                let p = i % rule.len();
                rpc.seek(SeekFrom::Start(p as u64)).expect("seek");

                thread::yield_now();

                let l = 5.min(rule.len() - p);
                let mut buf = vec![0u8; l];
                rpc.read_exact(&mut buf).expect("read_exact");
                assert_eq!(&buf[..], &rule[p..(p+l)]);
            }))
        }
        for t in threads {
            t.join().unwrap();
        }
    }

    fn is_send<T: Send>() -> bool { true }
    fn is_sync<T: Sync>() -> bool { true }

    #[test]
    fn test_send_sync() {
        assert!(is_send::<ReadPos<File>>());
        assert!(is_sync::<ReadPos<File>>());
    }

}
