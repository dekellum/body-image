//! Provides a uniform access strategy to HTTP bodies in RAM, or buffered to a
//! temporary file, and optionally memory mapped.
//!
//! [`BodyImage`](struct.BodyImage.html), [`BodySink`](struct.BodySink.html)
//! and supporting types provide a strategy for safely handling potentially
//! large HTTP request or response bodies without risk of allocation failure,
//! or the need to impose awkwardly low size limits in the face of high
//! concurrency. [`Tunables`](struct.Tunables.html) size thresholds can be
//! used to decide when to accumulate the body in RAM vs. the filesystem,
//! including when the length is unknown in advance.
//!
//! A [`Dialog`](struct.Dialog.html) defines a complete HTTP request and
//! response recording, using `BodyImage` for the request and response bodies
//! and _http_ crate types for the headers and other components.
//!
//! See the top-level (project workspace) [README][ws-readme] for additional
//! rationale.
//!
//! ## Optional Features
//!
//! The following features may be enabled or disabled at build time. All are
//! enabled by default.
//!
//! _mmap:_ Adds `BodyImage::mem_map` support for memory mapping from `FsRead`
//! state.
//!
//! ## Related Crates
//!
//! _[barc]:_ **B**ody **Arc**hive container file reader and writer, for
//! serializing `Dialog` records. See also _[barc-cli]_.
//!
//! _[body-image-futio]:_ Asynchronous HTTP integration with _futures_, _http_,
//! _hyper_ 0.12.x., and _tokio_ for `BodyImage`/`BodySink`, for both client
//! and server use.
//!
//! [ws-readme]: https://github.com/dekellum/body-image
//! [barc]: https://docs.rs/crate/barc
//! [barc-cli]: https://docs.rs/crate/barc-cli
//! [body-image-futio]: https://docs.rs/crate/body-image-futio

#![deny(dead_code, unused_imports)]
#![warn(rust_2018_idioms)]

use std::env;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut, BufMut};
use failure::Fail;

use http;
use log::{debug, warn};
use olio::io::GatheringReader;
use olio::fs::rc::{ReadPos, ReadSlice};
use tempfile::tempfile_in;

#[cfg(feature = "mmap")] use memmap::Mmap;
#[cfg(feature = "mmap")] use olio::mem::{MemAdvice, MemHandle};
#[cfg(feature = "mmap")] #[doc(hidden)] pub mod _mem_handle_ext;
#[cfg(feature = "mmap")] use _mem_handle_ext::MemHandleExt;

/// The crate version string.
pub static VERSION: &str               = env!("CARGO_PKG_VERSION");

/// Error enumeration for `BodyImage` and `BodySink` types.  This may be
/// extended in the future so exhaustive matching is gently discouraged with
/// an unused variant.
#[derive(Debug)]
pub enum BodyError {
    /// Error for when `Tunables::max_body` length is exceeded.
    BodyTooLong(u64),

    /// IO error associated with file creation/writes `FsWrite`, reads
    /// `FsRead`, or memory mapping.
    Io(io::Error),

    /// Unused variant to both enable non-exhaustive matching and warn against
    /// exhaustive matching.
    _FutureProof,
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            BodyError::BodyTooLong(l) =>
                write!(
                    f, "Body length of {}+ bytes exceeds Tunables::max_body",
                    l
                ),
            BodyError::Io(ref e) =>
                write!(f, "Body I/O: {}", e),
            BodyError::_FutureProof => unreachable!()
        }
    }
}

impl Fail for BodyError {
    fn cause(&self) -> Option<&dyn Fail> {
        match *self {
            BodyError::Io(ref e)     => Some(e),
            _ => None
        }
    }
}

impl From<io::Error> for BodyError {
    fn from(err: io::Error) -> BodyError {
        BodyError::Io(err)
    }
}

/// A logical buffer of bytes, which may or may not be RAM resident.
///
/// Besides a few immediate/convenience constructors found here, use
/// [`BodySink`](struct.BodySink.html) for the incremental or stream-oriented
/// collection of bytes to produce a `BodyImage`.
///
/// A `BodyImage` is always in one of the following states, as a buffering
/// strategy:
///
/// `Ram`
/// : A vector of zero, one, or many discontinuous (AKA scattered) byte
///   buffers in Random Access Memory. This state is also used to represent
///   an empty body (without allocation).
///
/// `FsRead`
/// : Body in a (temporary) file, ready for position based, sequential read.
///
/// `MemMap`
/// : Body in a memory mapped file, ready for random access read (default
///   *mmap* feature)
///
/// All states support concurrent reads. `BodyImage` is `Send` and supports
/// low-cost shallow `Clone` via internal (atomic) reference
/// counting. `BodyImage` is not `Sync` (with the default *mmap* feature
/// enabled).
#[derive(Clone, Debug)]
pub struct BodyImage {
    state: ImageState,
    len: u64
}

// Internal state enum for BodyImage
#[derive(Clone)]
enum ImageState {
    Ram(Vec<Bytes>),
    FsRead(Arc<File>),
    FsReadSlice(ReadSlice),
    #[cfg(feature = "mmap")]
    MemMap(MemHandle<Mmap>),
}

/// A logical buffer of bytes, which may or may not be RAM resident, in the
/// process of being written. This is the write-side corollary to
/// [`BodyImage`](struct.BodyImage.html).
///
/// A `BodySink` is always in one of the following states, as a buffering
/// strategy:
///
/// `Ram`
/// : A vector of zero, one, or many discontinuous (AKA scattered) byte
///   buffers in Random Access Memory. This state is also used to represent
///   an empty body (without allocation).
///
/// `FsWrite`
/// : Body being written to a (temporary) file.
#[derive(Debug)]
pub struct BodySink {
    state: SinkState,
    len: u64
}

enum SinkState {
    Ram(Vec<Bytes>),
    FsWrite(File),
}

impl SinkState {
    // Swap self with empty `Ram`, and return moved original.
    // Warning: Be careful about exposing an invalid length when using.
    fn cut(&mut self) -> Self {
        mem::replace(self, SinkState::Ram(Vec::with_capacity(0)))
    }
}

impl BodySink {
    /// Create new empty instance, which does not pre-allocate. The state is
    /// `Ram` with a zero-capacity vector.
    pub fn empty() -> BodySink {
        BodySink::with_ram_buffers(0)
    }

    /// Create a new `Ram` instance by pre-allocating a vector of buffers
    /// based on the given size estimate in bytes, assuming 8 KiB
    /// buffers. With a size_estimate of 0, this is the same as `empty`.
    pub fn with_ram(size_estimate: u64) -> BodySink {
        if size_estimate == 0 {
            BodySink::empty()
        } else {
            // Estimate buffers based an 8 KiB buffer size + 1.
            let cap = (size_estimate / 0x2000 + 1) as usize;
            BodySink::with_ram_buffers(cap)
        }
    }

    /// Create a new `Ram` instance by pre-allocating a vector of the
    /// specified capacity.
    pub fn with_ram_buffers(capacity: usize) -> BodySink {
        BodySink {
            state: SinkState::Ram(Vec::with_capacity(capacity)),
            len: 0
        }
    }

    /// Create a new instance in state `FsWrite`, using a new temporary file
    /// created in dir.
    pub fn with_fs<P>(dir: P) -> Result<BodySink, BodyError>
        where P: AsRef<Path>
    {
        let f = tempfile_in(dir)?;
        Ok(BodySink {
            state: SinkState::FsWrite(f),
            len: 0
        })
    }

    /// Return true if in state `Ram`.
    pub fn is_ram(&self) -> bool {
        match self.state {
            SinkState::Ram(_) => true,
            _ => false
        }
    }

    /// Return true if body is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Return the current length of body in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Save bytes by appending to `Ram` or writing to `FsWrite` file. When in
    /// state `Ram` this may be more efficient than `write_all` if
    /// `Into<Bytes>` doesn't copy.
    pub fn save<T>(&mut self, buf: T) -> Result<(), BodyError>
        where T: Into<Bytes>
    {
        let buf = buf.into();
        let len = buf.len() as u64;
        if len > 0 {
            match self.state {
                SinkState::Ram(ref mut v) => {
                    v.push(buf);
                }
                SinkState::FsWrite(ref mut f) => {
                    f.write_all(&buf)?;
                }
            }
            self.len += len;
        }
        Ok(())
    }

    /// Write all bytes to self.  When in state `FsWrite` this is copy free
    /// and more optimal than `save`.
    pub fn write_all<T>(&mut self, buf: T) -> Result<(), BodyError>
        where T: AsRef<[u8]>
    {
        let buf = buf.as_ref();
        let len = buf.len() as u64;
        if len > 0 {
            match self.state {
                SinkState::Ram(ref mut v) => {
                    v.push(buf.into());
                }
                SinkState::FsWrite(ref mut f) => {
                    f.write_all(buf)?;
                }
            }
            self.len += len;
        }
        Ok(())
    }

    /// If `Ram`, convert to `FsWrite` by writing all bytes in RAM to a
    /// temporary file, created in dir.  No-op if already `FsWrite`. Buffers
    /// are eagerly dropped as they are written. As a consequence, if any
    /// error result is returned (e.g. opening or writing to the file), self
    /// will be empty and in the `Ram` state. There is no practical recovery
    /// for the original body.
    pub fn write_back<P>(&mut self, dir: P) -> Result<&mut Self, BodyError>
        where P: AsRef<Path>
    {
        if self.is_ram() {
            if let SinkState::Ram(v) = self.state.cut() {
                let olen = self.len;
                self.len = 0;
                let mut f = tempfile_in(dir)?;
                for b in v {
                    f.write_all(&b)?;
                    drop::<Bytes>(b); // Ensure ASAP drop
                }
                self.state = SinkState::FsWrite(f);
                self.len = olen;
            }
        }
        Ok(self)
    }

    /// Consumes self, converts and returns as `BodyImage` ready for read.
    pub fn prepare(self) -> Result<BodyImage, BodyError> {
        match self.state {
            SinkState::Ram(v) => {
                Ok(BodyImage {
                    state: ImageState::Ram(v),
                    len: self.len
                })
            }
            SinkState::FsWrite(mut f) => {
                // Protect against empty files, which would fail if
                // mem_map'd, by replacing with empty `Ram` state
                if self.len == 0 {
                    Ok(BodyImage::empty())
                } else {
                    f.flush()?;
                    f.seek(SeekFrom::Start(0))?;
                    Ok(BodyImage {
                        state: ImageState::FsRead(Arc::new(f)),
                        len: self.len
                    })
                }
            }
        }
    }
}

impl Default for BodySink {
    fn default() -> BodySink { BodySink::empty() }
}

impl fmt::Debug for SinkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SinkState::Ram(ref v) => {
                // Avoids showing all buffers as u8 lists
                f.debug_struct("Ram(Vec<Bytes>)")
                    .field("capacity", &v.capacity())
                    .field("len", &v.len())
                    .finish()
            }
            SinkState::FsWrite(ref file) => {
                f.debug_tuple("FsWrite")
                    .field(file)
                    .finish()
            }
        }
    }
}

impl ImageState {
    fn empty() -> ImageState {
        ImageState::Ram(Vec::with_capacity(0))
    }

    // Swap self with empty `Ram`, and return moved original.
    // Warning: Be careful about exposing an invalid length when using.
    fn cut(&mut self) -> Self {
        mem::replace(self, ImageState::empty())
    }
}

impl BodyImage {
    /// Create new empty instance with no allocation. The state is
    /// `Ram` with a zero-capacity vector.
    pub fn empty() -> BodyImage {
        BodyImage {
            state: ImageState::empty(),
            len: 0
        }
    }

    /// Create a new `FsRead` instance based on an existing `File`. The fixed
    /// length is used to report `BodyImage::len` and may be obtained using
    /// `File::metadata`. If the provided length is zero, this returns as per
    /// `BodyImage::empty()` instead. Attempts to read from the returned
    /// `BodyImage` can fail if the file is not open for read.
    ///
    /// ### Safety
    ///
    /// Use of this constructor is potentially unsafe when the *mmap* feature
    /// enabled and once `mem_map` is called:
    ///
    /// * The `mem_map` call will fail if the file is zero length or not open
    /// for read.
    ///
    /// * Any concurrent writes to the file, or file system modifications
    /// while under use in `MemMap` state may lead to *Undefined Behavior*
    /// (UB).
    #[cfg(feature = "mmap")]
    pub unsafe fn from_file(file: File, length: u64) -> BodyImage {
        image_from_file(file, length)
    }

    /// Create a new `FsRead` instance based on an existing `File`. The fixed
    /// length is used to report `BodyImage::len` and may be obtained using
    /// `File::metadata`. If the provided length is zero, this returns as per
    /// `BodyImage::empty()` instead. Attempts to read from or `mem_map` the
    /// returned `BodyImage` can fail if the file is not open for read or is
    /// zero length.
    #[cfg(not(feature = "mmap"))]
    pub fn from_file(file: File, length: u64) -> BodyImage {
        image_from_file(file, length)
    }

    /// Create new instance from a single byte slice.
    pub fn from_slice<T>(bytes: T) -> BodyImage
        where T: Into<Bytes>
    {
        let mut bs = BodySink::with_ram_buffers(1);
        bs.save(bytes).expect("safe for Ram");
        bs.prepare().expect("safe for Ram")
    }

    /// Create a new instance based on a `ReadSlice`. The `BodyImage::len`
    /// will be as per `ReadSlice::len`, and if zero, this returns as per
    /// `BodyImage::empty()`. Attempts to read from the returned
    /// `BodyImage` can fail if the file is not open for read.
    ///
    /// ### Safety
    ///
    /// Use of this constructor is potentially unsafe when the *mmap* feature
    /// enabled and once `mem_map` is called:
    ///
    /// * The `mem_map` call will fail if the file is zero length or not open
    /// for read.
    ///
    /// * Any concurrent writes to the file, or file system modifications
    /// while under use in `MemMap` state may lead to *Undefined Behavior*
    /// (UB).
    #[cfg(feature = "mmap")]
    pub unsafe fn from_read_slice(rslice: ReadSlice) -> BodyImage {
        image_from_read_slice(rslice)
    }

    /// Create a new instance based on a `ReadSlice`. The `BodyImage::len`
    /// will be as per `ReadSlice::len`, and if zero, this returns as per
    /// `BodyImage::empty()`. Attempts to read from or `mem_map` the returned
    /// `BodyImage` can fail if the file is not open for read or is zero
    /// length.
    #[cfg(not(feature = "mmap"))]
    pub fn from_read_slice(rslice: ReadSlice) -> BodyImage {
        image_from_read_slice(rslice)
    }

    /// Return true if in state `Ram`.
    pub fn is_ram(&self) -> bool {
        match self.state {
            ImageState::Ram(_) => true,
            _ => false
        }
    }

    /// Return true if in state `MemMap`.
    #[cfg(feature = "mmap")]
    pub fn is_mem_map(&self) -> bool {
        match self.state {
            ImageState::MemMap(_) => true,
            _ => false
        }
    }

    /// Return the current length of body in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Return true if body is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// If `FsRead`, convert to `MemMap` by memory mapping the file.
    ///
    /// Under normal construction via `BodySink` in `FsWrite` state, this
    /// method is safe, because no other thread or process has access to the
    /// underlying file. Note the potential safety requirements via
    /// [`from_file`](#method-from_file) however.
    #[cfg(feature = "mmap")]
    pub fn mem_map(&mut self) -> Result<&mut Self, BodyError> {
        let map = match self.state {
            ImageState::FsRead(ref file) => {
                assert!(self.len > 0);
                unsafe { Mmap::map(&file) }?
            }
            ImageState::FsReadSlice(ref rslice) => {
                rslice.mem_map()?
            }
            _ => return Ok(self)
        };

        self.state = ImageState::MemMap(MemHandle::new(map));
        Ok(self)
    }

    /// If `Ram` with 2 or more buffers, *gather* by copying into a single
    /// contiguous buffer with the same total length. No-op for other
    /// states. Buffers are eagerly dropped as they are copied. Possibly in
    /// combination with `mem_map`, this can be used to ensure `Cursor` (and
    /// `&[u8]` slice) access via `reader`, at the cost of the copy.
    pub fn gather(&mut self) -> &mut Self {
        let scattered = if let ImageState::Ram(ref v) = self.state {
            v.len() > 1
        } else {
            false
        };

        if scattered {
            if let ImageState::Ram(v) = self.state.cut() {
                let mut newb = BytesMut::with_capacity(self.len as usize);
                for b in v {
                    newb.put_slice(&b);
                    drop::<Bytes>(b); // Ensure ASAP drop
                }
                let newb = newb.freeze();
                assert_eq!(newb.len() as u64, self.len);
                self.state = ImageState::Ram(vec![newb]);
            }
        }
        self
    }

    /// Return a new `BodyReader` enum over self. The enum provides a
    /// consistent `Read` reference, or can be destructured for access to
    /// the specific concrete types.
    pub fn reader(&self) -> BodyReader<'_> {
        match self.state {
            ImageState::Ram(ref v) => {
                if v.is_empty() {
                    BodyReader::Contiguous(Cursor::new(&[]))
                } else if v.len() == 1 {
                    BodyReader::Contiguous(Cursor::new(&v[0]))
                } else {
                    BodyReader::Scattered(GatheringReader::new(v))
                }
            }
            ImageState::FsRead(ref f) => {
                BodyReader::FileSlice(ReadSlice::new(f.clone(), 0, self.len))
            }
            ImageState::FsReadSlice(ref rslice) => {
                BodyReader::FileSlice(rslice.clone())
            }
            #[cfg(feature = "mmap")]
            ImageState::MemMap(ref m) => {
                BodyReader::Contiguous(Cursor::new(m))
            }
        }
    }

    /// Consume self, *exploding* into an
    /// [`ExplodedImage`](enum.ExplodedImage.html) variant.
    pub fn explode(self) -> ExplodedImage {
        match self.state {
            ImageState::Ram(v) => ExplodedImage::Ram(v),
            ImageState::FsRead(f) => {
                ExplodedImage::FsRead(ReadSlice::new(f, 0, self.len))
            }
            ImageState::FsReadSlice(rs) => ExplodedImage::FsRead(rs),
            #[cfg(feature = "mmap")]
            ImageState::MemMap(m) => ExplodedImage::MemMap(m),
        }
    }

    /// Given a `Read` object, a length estimate in bytes, and `Tunables` read
    /// and prepare a new `BodyImage`. `Tunables`, the estimate and actual
    /// length read will determine which buffering strategy is used. The
    /// length estimate provides a hint to use the file system from the start,
    /// which is more optimal than writing out accumulated `Ram` buffers
    /// later. If the length can't be estimated, use zero (0).
    pub fn read_from(r: &mut dyn Read, len_estimate: u64, tune: &Tunables)
        -> Result<BodyImage, BodyError>
    {
        if len_estimate > tune.max_body_ram() {
            let b = BodySink::with_fs(tune.temp_dir())?;
            return read_to_body_fs(r, b, tune);
        }

        let mut body = BodySink::with_ram(len_estimate);

        let mut size: u64 = 0;
        'eof: loop {
            let mut buf = BytesMut::with_capacity(tune.buffer_size_ram());
            'fill: loop {
                let len = match r.read(unsafe { buf.bytes_mut() }) {
                    Ok(len) => len,
                    Err(e) => {
                        if e.kind() == ErrorKind::Interrupted {
                            continue;
                        } else {
                            return Err(e.into());
                        }
                    }
                };
                if len == 0 {
                    break 'fill; // not 'eof as may have bytes in buf

                }
                unsafe { buf.advance_mut(len); }

                if buf.remaining_mut() < 1024 {
                    break 'fill;
                }
            }
            let len = buf.len() as u64;
            if len == 0 {
                break 'eof;
            }
            size += len;
            if size > tune.max_body() {
                return Err(BodyError::BodyTooLong(size));
            }
            if size > tune.max_body_ram() {
                body.write_back(tune.temp_dir())?;
                debug!("Write (Fs) buffer len {}", len);
                body.write_all(&buf)?;
                return read_to_body_fs(r, body, tune)
            }
            debug!("Saved (Ram) buffer len {}", len);
            body.save(buf.freeze())?;
        }
        let body = body.prepare()?;
        Ok(body)
    }

    /// Write self to `out` and return length. If `FsRead` this is performed
    /// using `std::io::copy` with `ReadPos` as input.
    pub fn write_to(&self, out: &mut dyn Write) -> Result<u64, BodyError> {
        match self.state {
            ImageState::Ram(ref v) => {
                for b in v {
                    out.write_all(b)?;
                }
            }
            #[cfg(feature = "mmap")]
            ImageState::MemMap(ref mh) => {
                mh.tmp_advise(MemAdvice::Sequential, || out.write_all(mh))?;
            }
            ImageState::FsRead(ref f) => {
                let mut rp = ReadPos::new(f.clone(), self.len);
                io::copy(&mut rp, out)?;
            }
            ImageState::FsReadSlice(ref rslice) => {
                let mut rs = rslice.clone();
                io::copy(&mut rs, out)?;
            }
        }
        Ok(self.len)
    }
}

// Create a new `FsRead` instance based on an existing `File`.
fn image_from_file(file: File, length: u64) -> BodyImage {
    if length > 0 {
        BodyImage {
            state: ImageState::FsRead(Arc::new(file)),
            len: length
        }
    } else {
        BodyImage::empty()
    }
}

// Create a new `FsRead` instance based on an existing `ReadSlice`.
fn image_from_read_slice(rslice: ReadSlice) -> BodyImage {
    let len = rslice.len();
    if len > 0 {
        BodyImage { state: ImageState::FsReadSlice(rslice), len }
    } else {
        BodyImage::empty()
    }
}

// Read all bytes from r, consume and write to a `BodySink` in state
// `FsWrite`, returning a final prepared `BodyImage`.
fn read_to_body_fs(r: &mut dyn Read, mut body: BodySink, tune: &Tunables)
    -> Result<BodyImage, BodyError>
{
    assert!(!body.is_ram());

    let mut size: u64 = 0;
    let mut buf = BytesMut::with_capacity(tune.buffer_size_fs());
    loop {
        let len = match r.read(unsafe { buf.bytes_mut() }) {
            Ok(l) => l,
            Err(e) => {
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                } else {
                    return Err(e.into());
                }
            }
        };
        if len == 0 {
            break;
        }
        unsafe { buf.advance_mut(len); }

        size += len as u64;
        if size > tune.max_body() {
            return Err(BodyError::BodyTooLong(size));
        }
        debug!("Write (Fs) buffer len {}", len);
        body.write_all(&buf)?;
        buf.clear();
    }
    let body = body.prepare()?;
    Ok(body)
}

impl Default for BodyImage {
    fn default() -> BodyImage { BodyImage::empty() }
}

impl fmt::Debug for ImageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ImageState::Ram(ref v) => {
                // Avoids showing all buffers as u8 lists
                f.debug_struct("Ram(Vec<Bytes>)")
                    .field("capacity", &v.capacity())
                    .field("len", &v.len())
                    .finish()
            }
            ImageState::FsRead(ref file) => {
                f.debug_tuple("FsRead")
                    .field(file)
                    .finish()
            }
            ImageState::FsReadSlice(ref rslice) => {
                f.debug_tuple("FsReadSlice")
                    .field(rslice)
                    .finish()
            }
            #[cfg(feature = "mmap")]
            ImageState::MemMap(ref m) => {
                f.debug_tuple("MemMap")
                    .field(m)
                    .finish()
            }
        }
    }
}

/// *Exploded* representation of the possible `BodyImage` states, obtained via
/// [`BodyImage::explode`](struct.BodyImage.html#method.explode).
pub enum ExplodedImage {
    Ram(Vec<Bytes>),
    FsRead(ReadSlice),
    #[cfg(feature = "mmap")]
    MemMap(MemHandle<Mmap>),
}

/// Provides a `Read` reference for a `BodyImage` in any state.
pub enum BodyReader<'a> {
    /// `Cursor` over a contiguous single RAM buffer, from `Ram` or
    /// `MemMap`. Also used for the empty case. `Cursor::into_inner` may be
    /// used for direct access to the memory byte slice.
    Contiguous(Cursor<&'a [u8]>),

    /// `GatheringReader` providing `Read` over 2 or more scattered RAM
    /// buffers.
    Scattered(GatheringReader<'a, Bytes>),

    /// `ReadSlice` providing instance independent, unbuffered `Read` and
    /// `Seek` for BodyImage `FsRead` state, limited to a range within an
    /// underlying file. Consider wrapping this in `std::io::BufReader` if
    /// performing many small reads.
    FileSlice(ReadSlice),
}

impl<'a> BodyReader<'a> {
    /// Return the `Read` reference.
    pub fn as_read(&mut self) -> &mut dyn Read {
        match *self {
            BodyReader::Contiguous(ref mut cursor) => cursor,
            BodyReader::Scattered(ref mut gatherer) => gatherer,
            BodyReader::FileSlice(ref mut rslice) => rslice,
        }
    }
}

/// Extract of an HTTP request.
///
/// Alternate spelling of _prologue_.
#[derive(Clone, Debug)]
pub struct Prolog {
    pub method:       http::Method,
    pub url:          http::Uri,
    pub req_headers:  http::HeaderMap,
    pub req_body:     BodyImage,
}

/// Extract of an HTTP response.
#[derive(Clone, Debug)]
pub struct Epilog {
    pub version:      http::Version,
    pub status:       http::StatusCode,
    pub res_headers:  http::HeaderMap,
    pub res_body:     BodyImage,
    pub res_decoded:  Vec<Encoding>,
}

/// A subset of supported HTTP Transfer or Content-Encoding values. The
/// `Display`/`ToString` representation is as per the HTTP header value.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Encoding {
    Chunked,
    Deflate,
    Gzip,
    Brotli,
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            Encoding::Chunked => "chunked",
            Encoding::Deflate => "deflate",
            Encoding::Gzip    => "gzip",
            Encoding::Brotli  => "br",
        })
    }
}

/// An HTTP request and response recording.
///
/// This composed type has private fields but offers getters, mutable getters
/// and setter methods. Several import getter methods are are found in trait
/// implementations [`RequestRecorded`](#impl-RequestRecorded) and
/// [`Recorded`](#impl-Recorded).
///
/// It may be constructed via the `Prolog` and `Epilog` public structs and the
/// [`explode`](#method.explode) method used to extract the same.
#[derive(Clone, Debug)]
pub struct Dialog {
    pro: Prolog,
    epi: Epilog,
}

/// Access by reference for HTTP request recording types.
pub trait RequestRecorded {
    /// Map of HTTP request headers.
    fn req_headers(&self) -> &http::HeaderMap;

    /// Request body (e.g for HTTP POST, etc.) which may or may not be RAM
    /// resident.
    fn req_body(&self)    -> &BodyImage;
}

/// Access by reference for HTTP request (via `RequestRecorded`) and response
/// recording types.
pub trait Recorded: RequestRecorded {
    /// Map of HTTP response headers.
    fn res_headers(&self) -> &http::HeaderMap;

    /// Response body which may or may not be RAM resident.
    fn res_body(&self)    -> &BodyImage;
}

impl Dialog {
    /// Construct from a `Prolog` and `Epilog`.
    pub fn new(pro: Prolog, epi: Epilog) -> Dialog {
        Dialog { pro, epi }
    }

    /// The HTTP method (verb), e.g. `GET`, `POST`, etc.
    pub fn method(&self)      -> &http::Method         { &self.pro.method }

    /// The complete URL as used in the request.
    pub fn url(&self)         -> &http::Uri            { &self.pro.url }

    /// A mutable reference to the request body. This is primarly provided
    /// to allow state mutating operations such as `BodyImage::mem_map`.
    pub fn req_body_mut(&mut self) -> &mut BodyImage {
        &mut self.pro.req_body
    }

    /// The response status code.
    pub fn res_status(&self)  -> http::StatusCode      { self.epi.status }

    /// The response HTTP version.
    pub fn res_version(&self) -> http::Version         { self.epi.version }

    /// A list of encodings that were removed (decoded) to provide this
    /// representation of the response body (`res_body`). May be empty.
    pub fn res_decoded(&self) -> &Vec<Encoding>        { &self.epi.res_decoded }

    /// A mutable reference to the response body. This is primarly provided
    /// to allow state mutating operations such as `BodyImage::mem_map`.
    pub fn res_body_mut(&mut self) -> &mut BodyImage   { &mut self.epi.res_body }

    /// Set a new response body and decoded vector.
    pub fn set_res_body_decoded(&mut self, body: BodyImage, decoded: Vec<Encoding>) {
        self.epi.res_body = body;
        self.epi.res_decoded = decoded;
    }

    /// Consume self, *exploding* into a (`Prolog`, `Epilog`) tuple (each
    /// with public fields).
    pub fn explode(self) -> (Prolog, Epilog) {
        (self.pro, self.epi)
    }
}

impl RequestRecorded for Dialog {
    fn req_headers(&self) -> &http::HeaderMap      { &self.pro.req_headers }
    fn req_body(&self)    -> &BodyImage            { &self.pro.req_body }
}

impl Recorded for Dialog {
    fn res_headers(&self) -> &http::HeaderMap      { &self.epi.res_headers }
    fn res_body(&self)    -> &BodyImage            { &self.epi.res_body }
}

/// A collection of size limits and performance tuning constants. Setters are
/// available via [`Tuner`](struct.Tuner.html)
#[derive(Debug, Clone)]
pub struct Tunables {
    max_body_ram:            u64,
    max_body:                u64,
    buffer_size_ram:         usize,
    buffer_size_fs:          usize,
    size_estimate_deflate:   u16,
    size_estimate_gzip:      u16,
    size_estimate_brotli:    u16,
    temp_dir:                PathBuf,
    res_timeout:             Option<Duration>,
    body_timeout:            Option<Duration>,
}

impl Tunables {
    /// Construct with default values.
    pub fn new() -> Tunables {
        Tunables {
            max_body_ram:       192 * 1024,
            max_body:   1024 * 1024 * 1024,
            buffer_size_ram:      8 * 1024,
            buffer_size_fs:      64 * 1024,
            size_estimate_deflate:       4,
            size_estimate_gzip:          5,
            size_estimate_brotli:        6,
            temp_dir:     env::temp_dir(),
            res_timeout:  None,
            body_timeout: Some(Duration::from_secs(60)),
        }
    }

    /// Return the maximum body size in bytes allowed in RAM, e.g. before
    /// writing to a temporary file, or memory mapping instead of direct, bulk
    /// read. Default: 192 KiB.
    pub fn max_body_ram(&self) -> u64 {
        self.max_body_ram
    }

    /// Return the maximum body size in bytes allowed in any form (RAM or
    /// file). Default: 1 GiB.
    pub fn max_body(&self) -> u64 {
        self.max_body
    }

    /// Return the buffer size in bytes to use when buffering to RAM.
    /// Default: 8 KiB.
    pub fn buffer_size_ram(&self) -> usize {
        self.buffer_size_ram
    }

    /// Return the buffer size in bytes to use when buffering to/from
    /// the file-system. Default: 64 KiB.
    pub fn buffer_size_fs(&self) -> usize {
        self.buffer_size_fs
    }

    /// Return the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _deflate_ compression algorithm.  Default: 4.
    pub fn size_estimate_deflate(&self) -> u16 {
        self.size_estimate_deflate
    }

    /// Return the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _gzip_ compression algorithm.  Default: 5.
    pub fn size_estimate_gzip(&self) -> u16 {
        self.size_estimate_gzip
    }

    /// Return the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _Brotli_ compression algorithm.  Default: 6.
    pub fn size_estimate_brotli(&self) -> u16 {
        self.size_estimate_brotli
    }

    /// Return the directory path in which to write temporary (`BodyImage`)
    /// files.  Default: `std::env::temp_dir()`
    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    /// Return the maximum initial response timeout interval.
    /// Default: None (e.g. unset)
    pub fn res_timeout(&self) -> Option<Duration> {
        self.res_timeout
    }

    /// Return the maximum streaming body timeout interval.
    /// Default: 60 seconds
    pub fn body_timeout(&self) -> Option<Duration> {
        self.body_timeout
    }
}

impl Default for Tunables {
    fn default() -> Self { Tunables::new() }
}

/// A builder for `Tunables`. Invariants are asserted in the various setters
/// and `finish`.
#[derive(Clone)]
pub struct Tuner {
    template: Tunables
}

impl Tuner {
    /// New `Tuner` with all `Tunables` defaults.
    pub fn new() -> Tuner {
        Tuner { template: Tunables::new() }
    }

    /// Set the maximum body size in bytes allowed in RAM.
    pub fn set_max_body_ram(&mut self, size: u64) -> &mut Tuner {
        self.template.max_body_ram = size;
        self
    }

    /// Set the maximum body size in bytes allowed in any form (RAM or
    /// file). This must be at least as large as `max_body_ram`, as asserted
    /// on `finish`.
    pub fn set_max_body(&mut self, size: u64) -> &mut Tuner {
        self.template.max_body = size;
        self
    }

    /// Set the buffer size in bytes to use when buffering to RAM.
    pub fn set_buffer_size_ram(&mut self, size: usize) -> &mut Tuner {
        assert!(size > 0, "buffer_size_ram must be greater than zero");
        self.template.buffer_size_ram = size;
        self
    }

    /// Set the buffer size in bytes to use when buffering to/from
    /// the file-system.
    pub fn set_buffer_size_fs(&mut self, size: usize) -> &mut Tuner {
        assert!(size > 0, "buffer_size_fs must be greater than zero");
        self.template.buffer_size_fs = size;
        self
    }

    /// Set the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _deflate_ compression algorithm.
    pub fn set_size_estimate_deflate(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_deflate must be >= 1");
        self.template.size_estimate_deflate = multiple;
        self
    }

    /// Set the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _gzip_ compression algorithm.
    pub fn set_size_estimate_gzip(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_gzip must be >= 1");
        self.template.size_estimate_gzip = multiple;
        self
    }

    /// Set the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _Brotli_ compression algorithm.
    pub fn set_size_estimate_brotli(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_brotli must be >= 1");
        self.template.size_estimate_brotli = multiple;
        self
    }

    /// Set the path in which to write temporary files.
    pub fn set_temp_dir<P>(&mut self, path: P) -> &mut Tuner
        where P: AsRef<Path>
    {
        self.template.temp_dir = path.as_ref().into();
        self
    }

    /// Set the maximum initial response timeout interval.
    pub fn set_res_timeout(&mut self, dur: Duration) -> &mut Tuner {
        self.template.res_timeout = Some(dur);
        self
    }

    /// Unset (e.g. disable) response timeout
    pub fn unset_res_timeout(&mut self) -> &mut Tuner {
        self.template.res_timeout = None;
        self
    }

    /// Set the maximum streaming body timeout interval.
    pub fn set_body_timeout(&mut self, dur: Duration) -> &mut Tuner {
        self.template.body_timeout = Some(dur);
        self
    }

    /// Unset (e.g. disable) body timeout
    pub fn unset_body_timeout(&mut self) -> &mut Tuner {
        self.template.body_timeout = None;
        self
    }

    /// Finish building, asserting any remaining invariants, and return a new
    /// `Tunables` instance.
    pub fn finish(&self) -> Tunables {
        let t = self.template.clone();
        assert!(t.max_body_ram <= t.max_body,
                "max_body_ram can't be greater than max_body");
        if t.res_timeout.is_some() && t.body_timeout.is_some() {
            assert!(t.res_timeout.unwrap() <= t.body_timeout.unwrap(),
                    "res_timeout can't be greater than body_timeout");
        }
        t
    }
}

impl Default for Tuner {
    fn default() -> Self { Tuner::new() }
}

#[cfg(test)]
mod body_tests {
    use super::*;

    fn is_send<T: Send>() -> bool { true }
    fn is_sync<T: Sync>() -> bool { true }

    #[test]
    fn test_send_sync() {
        assert!(is_send::<BodyImage>());
        assert!(is_send::<Dialog>());

        assert!(is_send::<Tunables>());
        assert!(is_sync::<Tunables>());

        assert!(is_send::<BodyReader<'_>>());
        assert!(is_sync::<BodyReader<'_>>());
    }

    #[cfg(not(feature = "mmap"))]
    #[test]
    fn test_sync_not_mmap() {
        assert!(is_sync::<BodyImage>());
        assert!(is_sync::<Dialog>());
    }

    #[test]
    fn test_empty_read() {
        let body = BodyImage::empty();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = Vec::new();
        br.read_to_end(&mut obuf).unwrap();
        assert!(obuf.is_empty());
    }

    #[test]
    fn test_contiguous_read() {
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello world").unwrap();
        let body = body.prepare().unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_scattered_read() {
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        let body = body.prepare().unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_scattered_read_clone() {
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        let mut body = body.prepare().unwrap();
        let body_clone = body.clone();
        body.gather();

        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);

        let mut body_reader = body_clone.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_scattered_gather() {
        let mut body = BodySink::with_ram_buffers(2);
        body.save(&b"hello"[..]).unwrap();
        body.save(&b" "[..]).unwrap();
        body.save(&b"world"[..]).unwrap();
        let mut body = body.prepare().unwrap();
        body.gather();
        if let BodyReader::Contiguous(cursor) = body.reader() {
            let bslice = cursor.into_inner();
            assert_eq!(b"hello world", bslice);
        } else {
            panic!("not contiguous?!");
        }
    }

    #[test]
    fn test_read_from() {
        let tune = Tunables::new();
        let salutation = b"hello world";
        let mut src = Cursor::new(salutation);
        let body = BodyImage::read_from(&mut src, 0, &tune).unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = Vec::new();
        br.read_to_end(&mut obuf).unwrap();
        assert_eq!(salutation, &obuf[..]);
    }

    #[test]
    fn test_read_from_too_long() {
        let tune = Tuner::new()
            .set_max_body_ram(6)
            .set_max_body(6)
            .finish();
        let salutation = b"hello world";
        let mut src = Cursor::new(salutation);
        if let Err(e) = BodyImage::read_from(&mut src, 0, &tune) {
            if let BodyError::BodyTooLong(l) = e {
                assert_eq!(l, salutation.len() as u64)
            } else {
                panic!("Other error: {}", e);
            }
        } else {
            panic!("Read from, too long, success!?");
        }
    }

    #[test]
    fn test_fs_read() {
        let tune = Tunables::new();
        let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        let body = body.prepare().unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_fs_back_read() {
        let tune = Tunables::new();
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        body.write_back(tune.temp_dir()).unwrap();
        let body = body.prepare().unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_w_back_w_read() {
        let tune = Tunables::new();
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_back(tune.temp_dir()).unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        let body = body.prepare().unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_fs_back_fail() {
        let tune = Tuner::new().set_temp_dir("./no-existe/").finish();
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        if body.write_back(tune.temp_dir()).is_err() {
            assert!(body.is_ram());
            assert_eq!(body.len(), 0);
        } else {
            panic!("write_back with bogus dir success?!");
        }
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn test_fs_map_read() {
        let tune = Tunables::new();
        let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        let mut body = body.prepare().unwrap();
        body.mem_map().unwrap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[test]
    fn test_fs_back_read_clone() {
        let tune = Tunables::new();
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        body.write_back(tune.temp_dir()).unwrap();
        let body = body.prepare().unwrap();

        {
            let mut body_reader = body.reader();
            let br = body_reader.as_read();
            let mut obuf = String::new();
            br.read_to_string(&mut obuf).unwrap();
            assert_eq!("hello world", &obuf[..]);
        }

        let body_clone_1 = body.clone();
        let body_clone_2 = body_clone_1.clone();

        let mut body_reader = body_clone_1.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);

        let mut body_reader = body.reader(); // read original (prepared) body
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);

        let mut body_reader = body_clone_2.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn test_fs_map_clone_shared() {
        let tune = Tunables::new();
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        body.write_back(tune.temp_dir()).unwrap();
        let mut body = body.prepare().unwrap();
        body.mem_map().unwrap();
        println!("{:?}", body);

        let ptr1 = if let BodyReader::Contiguous(cursor) = body.reader() {
            let bslice = cursor.into_inner();
            assert_eq!(b"hello world", bslice);
            format!("{:p}", bslice)
        } else {
            panic!("not contiguous?!");
        };

        let body_clone = body.clone();
        println!("{:?}", body_clone);

        let ptr2 = if let BodyReader::Contiguous(cursor) = body_clone.reader() {
            let bslice = cursor.into_inner();
            assert_eq!(b"hello world", bslice);
            format!("{:p}", bslice)
        } else {
            panic!("not contiguous?!");
        };

        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn test_fs_empty() {
        let tune = Tunables::new();
        let body = BodySink::with_fs(tune.temp_dir()).unwrap();
        let body = body.prepare().unwrap();
        assert!(body.is_empty());
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn test_fs_map_empty() {
        let tune = Tunables::new();
        let body = BodySink::with_fs(tune.temp_dir()).unwrap();
        let mut body = body.prepare().unwrap();
        body.mem_map().unwrap();
        assert!(body.is_empty());
    }
}
