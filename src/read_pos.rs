//! Utility for independent `Read` instances of `FsRead`.

use std::fs::File;
use std::io;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::FileExt;

#[cfg(windows)]
use std::os::windows::fs::FileExt;

/// Trait offering a uniform `pread` for positioned reads, with platform
/// dependent side-effects.
pub trait PosRead {
    /// Read some bytes, starting at the specified offset, into the specified
    /// buffer and return the number of bytes read. The offset is absolute,
    /// from the beginning of the underlying file.  The position of the
    /// underlying file pointer (aka cursor) is not used. It is platform
    /// dependent whether the underlying file pointer is modified by this
    /// operation.
    fn pread(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

/// Re-implements `Read` and `Seek` over a shared `File` reference using
/// _only_ positioned reads via `FileExt` (platform specific)
/// `read_at`/`seek_read`, and by maintaining an instance independent
/// position.
///
/// `ReadPos` supports independent instances on the same `File` (and handle)
/// without needing a path to open an independent `File`.  Thus it is
/// compatible with "unnamed" (not linked) temporary files. Note that unix
/// `dup`/`dup2` and `File::try_clone` do not provide independent file
/// positions.
///
/// ### Current Limitations
///
/// * On some platforms (e.g. Windows) the underlying file position will be
/// modified as `ReadPos` is read. This will not effect this or other
/// `ReadPos` instances however.
///
/// * A (file) length is arbitrarily passed in and used solely for handling
/// SeekFrom::End. Seeking past end of file is allowed by the platforms and
/// `File` in any case.  This size is not checked or updated via `File`
/// metadata, which could result in surprising behavior if concurrent updates
/// were possible.
#[derive(Debug)]
pub struct ReadPos {
    pos: u64,
    length: u64,
    file: Arc<File>,
}

impl PosRead for File {
    #[cfg(unix)]
    #[inline]
    fn pread(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.read_at(buf, offset)
    }

    #[cfg(windows)]
    #[inline]
    fn pread(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.seek_read(buf, offset)
    }
}

impl ReadPos {
    /// New instance by `File` reference and fixed file length.
    pub(crate) fn new(file: Arc<File>, length: u64) -> ReadPos {
        ReadPos { pos: 0, length, file }
    }

    /// Seek by signed offset from an origin, checking for underflow and
    /// overflow.
    fn seek_from(&mut self, origin: u64, offset: i64) -> io::Result<u64> {
        let checked_pos = if offset < 0 {
            origin.checked_sub((-offset) as u64)
        } else {
            origin.checked_add(offset as u64)
        };

        if let Some(p) = checked_pos {
            self.pos = p;
            Ok(p)
        } else if offset < 0 {
            Err(Error::new(
                ErrorKind::InvalidInput,
                "Attempted seek to a negative absolute position"
            ))
        } else {
            Err(Error::new(
                ErrorKind::Other,
                "Attempted seek would overflow u64 position"
            ))
        }
    }
}

impl Clone for ReadPos {
    /// Return a new, independent `ReadPos` with the same length and file
    /// reference as self, and with position 0 (ignore self's current
    /// position).
    fn clone(&self) -> ReadPos {
        ReadPos { pos: 0, length: self.length, file: self.file.clone() }
    }
}

impl PosRead for ReadPos {
    #[inline]
    fn pread(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.file.pread(buf, offset)
    }
}

impl Read for ReadPos {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.pread(buf, self.pos)?;
        self.pos += len as u64;
        Ok(len)
    }
}

impl Seek for ReadPos {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match from {
            SeekFrom::Start(p) => {
                self.pos = p;
                Ok(p)
            }
            SeekFrom::End(offset) => {
                let origin = self.length;
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
    use std::io::{BufReader, Read, Write};
    use std::thread;
    use ::tempfile::tempfile;
    use super::*;

    #[test]
    fn test_seek() {
        let mut f = tempfile().unwrap();
        f.write_all(b"1234567890").unwrap();

        let mut r1 = ReadPos::new(Arc::new(f), 10);
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
    fn test_with_buf_reader() {
        let mut f = tempfile().unwrap();
        f.write_all(b"1234567890").unwrap();

        let r0 = ReadPos::new(Arc::new(f), 10);
        let mut r1 = BufReader::with_capacity(0x2000, r0);
        let mut buf = [0u8; 5];

        let p = r1.seek(SeekFrom::Start(1)).unwrap();
        assert_eq!(1, p);
        r1.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"23456");

        let mut r0 = r1.into_inner();
        let p = r0.seek(SeekFrom::Current(0)).unwrap();
        assert_eq!(10, p);

        let l = r0.read(&mut buf).unwrap();
        assert_eq!(0, l);
    }

    #[test]
    fn test_interleaved() {
        let mut f = tempfile().unwrap();
        f.write_all(b"1234567890").unwrap();

        let mut r1 = ReadPos::new(Arc::new(f), 10);

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

    #[test]
    fn test_concurrent_seek_read() {
        let mut f = tempfile().unwrap();
        let rule = b"1234567890";
        f.write_all(rule).unwrap();
        let f = Arc::new(f);

        let mut threads = Vec::with_capacity(30);
        for i in 0..50 {
            let mut rpc = ReadPos::new(f.clone(), rule.len() as u64);
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
        assert!(is_send::<ReadPos>());
        assert!(is_sync::<ReadPos>());
    }
}
