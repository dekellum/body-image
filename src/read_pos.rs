//! Utilities for position independent file access.
//!
//! The `PosRead` trait offers a uniform `pread` for positioned reads.
//!
//! The `ReadPos` and `ReadSlice` types support multiple independent instance
//! positions over a shared `File`, without needing a path to open an
//! independent new `File` instance.  Thus they are compatible with "unnamed"
//! (not linked) temporary files, and can reduce the number of necessary file
//! handles.  Note that unix `dup`/`dup2` and the standard `File::try_clone`
//! do _not_ provide independent file positions.

use std::fs::File;
use std::io;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::FileExt;

#[cfg(windows)]
use std::os::windows::fs::FileExt;

#[cfg(feature = "memmap")]
use memmap::{Mmap, MmapOptions};

/// Trait offering a uniform `pread` for positioned reads, with platform
/// dependent side-effects.
pub trait PosRead {
    /// Read some bytes, starting at the specified offset, into the specified
    /// buffer and return the number of bytes read. The offset is from the
    /// start of the underlying file or file range.  The position of the
    /// underlying file pointer (aka cursor) is not used. It is platform
    /// dependent whether the underlying file pointer is modified by this
    /// operation.
    fn pread(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

/// Re-implements `Read` and `Seek` over a shared `File` reference using
/// _only_ positioned reads, and by maintaining an instance independent
/// position.
///
/// A fixed `length` is passed on construction and used solely for handling
/// `SeekFrom::End`. Reads are not constrained by this length. The length is
/// neither checked nor updated via file metadata, and could deviate from the
/// underlying file length if concurrent writes or truncation is possible.
#[derive(Debug)]
pub struct ReadPos {
    pos: u64,
    length: u64,
    file: Arc<File>,
}

/// Re-implements `Read` and `Seek` over a shared `File` reference using
/// _only_ positioned reads, and by maintaining instance independent start,
/// end, and position.
///
/// As compared with [`ReadPos`](struct.ReadPos.html), `ReadSlice` adds a
/// general start offset, and limits access to the start..end range. Seeks are
/// relative, so a seek to `SeekFrom::Start(0)` is always the first byte of
/// the slice.
///
/// Fixed `start` and `end` offsets are passed on construction and used to
/// constrain reads and interpret `SeekFrom::End`. Seeking past end of file is
/// allowed by the platforms and `File` in any case.  These offsets are
/// neither checked nor updated via file metadata, and the end offset could
/// deviate from the underlying file length if concurrent writes or truncation
/// is possible.
#[derive(Debug)]
pub struct ReadSlice {
    start: u64,
    pos: u64,
    end: u64,
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
    pub fn new(file: Arc<File>, length: u64) -> ReadPos {
        ReadPos { pos: 0, length, file }
    }

    /// Return the length as provided on construction. This may differ from
    /// the underlying file length.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Return `true` if length is 0.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Return a new and independent `ReadSlice` for the same file, for the
    /// range of byte offsets `start..end`. Panics if start is greater than
    /// end. Note that end is not checked against the constructed length.
    pub fn subslice(&self, start: u64, end: u64) -> ReadSlice {
        ReadSlice::new(self.file.clone(), start, end)
    }

    /// Return the current instance position. This is a convenience shorthand
    /// for `seek(SeekFrom::Current(0))`, is infallable, and does not require
    /// a mutable reference.
    pub fn tell(&self) -> u64 {
        self.pos
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
    /// reference as self, and with position 0 (ignores the current
    /// position of self).
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
    #[inline]
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

impl ReadSlice {
    /// New instance by `File` reference, fixed start and end offsets.
    pub fn new(file: Arc<File>, start: u64, end: u64) -> ReadSlice {
        assert!(start <= end);
        ReadSlice { start, pos: start, end, file }
    }

    /// Return the total size of the slice in bytes. This is based on the
    /// start and end offsets as constructed and can differ from the
    /// underlying file length.
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    /// Return `true` if length is 0.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return a new read-only memory map handle `Mmap` for the complete
    /// region of the underlying file, from start to end.
    #[cfg(feature = "memmap")]
    pub(crate) fn mem_map(&self) -> Result<Mmap, io::Error> {
        let offset = self.start;
        let len = self.len();
        // See: https://github.com/danburkert/memmap-rs/pull/65
        assert!(offset <= usize::max_value() as u64);
        assert!(len    <= usize::max_value() as u64);
        assert!(len > 0);
        unsafe {
            MmapOptions::new()
                .offset(offset as usize)
                .len(len as usize)
                .map(&self.file)
        }
    }

    /// Return a new and independent `ReadSlice` for the same file, for the
    /// range of byte offsets `start..end` which are relative to, and must be
    /// fully contained by self. Checks for and panics on overflow, if
    /// start..end is not fully contained, or if start is greater-than end.
    pub fn subslice(&self, start: u64, end: u64) -> ReadSlice {
        let abs_start = self.start.checked_add(start)
            .expect("ReadSlice::subslice start overflow");
        let abs_end = self.start.checked_add(end)
            .expect("ReadSlice::subslice end overflow");
        assert!(abs_start  <= abs_end);
        assert!(self.start <= abs_start);
        assert!(self.end   >= abs_end);

        ReadSlice::new(self.file.clone(), abs_start, abs_end)
    }

    /// Return the current instance position, relative to the slice. This is a
    /// convenience shorthand for `seek(SeekFrom::Current(0))`, is infallable,
    /// and does not require a mutable reference.
    pub fn tell(&self) -> u64 {
        self.pos - self.start
    }

    /// Like `PosRead::pread`, but using an absolute (internal) position
    /// instead of the external, relative offset.
    fn pread_abs(&self, buf: &mut [u8], abspos: u64) -> io::Result<usize> {
        if abspos < self.end {
            let mlen = self.end - abspos; // positive/no-underflow per above
            if (buf.len() as u64) <= mlen {
                self.file.pread(buf, abspos)
            } else {
                // safe cast: mlen < buf.len which is already usize
                self.file.pread(&mut buf[..(mlen as usize)], abspos)
            }
        } else {
            Ok(0)
        }
    }

    /// Seek by signed offset from an (absolute) origin, checking for
    /// underflow and overflow.
    fn seek_from(&mut self, origin: u64, offset: i64) -> io::Result<u64> {
        let checked_pos = if offset < 0 {
            origin.checked_sub((-offset) as u64)
        } else {
            origin.checked_add(offset as u64)
        };

        if let Some(p) = checked_pos {
            self.seek_to(p)
        } else if offset < 0 {
            Err(Error::new(
                ErrorKind::InvalidInput,
                "Attempted seek to a negative position"
            ))
        } else {
            Err(Error::new(
                ErrorKind::Other,
                "Attempted seek would overflow u64 position"
            ))
        }
    }

    /// Seek by absolute position, validated with the start index. Return the
    /// new relative position, or Error if the absolute position is before
    /// start. Like with a regular File, positions beyond end are allowed, and
    /// this is checked on reads.
    fn seek_to(&mut self, abspos: u64) -> io::Result<u64> {
        if abspos < self.start {
            Err(Error::new(
                ErrorKind::InvalidInput,
                "Attempted seek to a negative position"
            ))
        } else {
            self.pos = abspos;
            Ok(abspos - self.start)
        }
    }
}

impl Clone for ReadSlice {
    /// Return a new, independent `ReadSlice` with the same start, end and
    /// file reference as self, and positioned at start (ignores the current
    /// position of self).
    fn clone(&self) -> ReadSlice {
        ReadSlice { start: self.start,
                    pos:   self.start,
                    end:   self.end,
                    file:  self.file.clone() }
    }
}

impl PosRead for ReadSlice {
    #[inline]
    fn pread(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let pos = self.start.saturating_add(offset);
        if pos < self.end {
            self.pread_abs(buf, pos)
        } else {
            Ok(0)
        }
    }
}

impl Read for ReadSlice {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.pread_abs(buf, self.pos)?;
        self.pos += len as u64;
        Ok(len)
    }
}

impl Seek for ReadSlice {
    /// Seek to an offset, in bytes, in a stream. In this implementation,
    /// seeks are relative to the fixed starting offset to underlying File, so
    /// a seek to `SeekFrom::Start(0)` is always the first byte of the slice.
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match from {
            SeekFrom::Start(p) => {
                if let Some(p) = self.start.checked_add(p) {
                    self.seek_to(p)
                } else {
                    Err(Error::new(
                        ErrorKind::Other,
                        "Attempted seek would overflow u64 position"
                    ))
                }
            },
            SeekFrom::End(offset) => {
                let origin = self.end;
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

    #[test]
    fn test_slice_seek() {
        let mut f = tempfile().unwrap();
        f.write_all(b"1234567890").unwrap();

        let mut r1 = ReadSlice::new(Arc::new(f), 0, 10);
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
    fn test_slice_seek_offset() {
        let mut f = tempfile().unwrap();
        f.write_all(b"012345678901").unwrap();

        let r1 = ReadSlice::new(Arc::new(f), 1, 12);
        let mut r1 = r1.subslice(0, 10);

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
    fn test_slice_with_buf_reader() {
        let mut f = tempfile().unwrap();
        f.write_all(b"01234567890").unwrap();

        let r0 = ReadSlice::new(Arc::new(f), 1, 11);
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

    fn is_send<T: Send>() -> bool { true }
    fn is_sync<T: Sync>() -> bool { true }

    #[test]
    fn test_send_sync() {
        assert!(is_send::<ReadPos>());
        assert!(is_sync::<ReadPos>());
        assert!(is_send::<ReadSlice>());
        assert!(is_sync::<ReadSlice>());
    }
}
