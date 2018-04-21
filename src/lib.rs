//! This crate provides a few separately usable but closely related HTTP
//! ecosystem components.
//!
//! In the _root_ module, the [`BodyImage`](struct.BodyImage.html) struct
//! and supporting types provides a strategy for safely handling
//! potentially large HTTP request or response bodies without risk of
//! allocation failure, or the need to impose awkwardly low size limits in the
//! face of high concurrency. `Tunables` size thresholds can be used to decide
//! when to accumulate the body in RAM vs the filesystem, including when the
//! length is unknown in advance.
//!
//! See the top level README for additional rationale.
//!
//! A [`Dialog`](struct.Dialog.html) defines a complete HTTP request and
//! response recording, using `BodyImage` for the request and response bodies
//! and _http_ crate types for the headers and other components.
//!
//! The [_barc_ module](barc/index.html) defines a container file format, reader
//! and writer for dialog records. This has broad use cases, from convenient
//! test fixtures for web applications, to caching and web crawling.
//!
//! ## Optional Features
//!
//! The following features may be enabled or disabled at build time:
//!
//! _client (non-default):_ The [client module](client/index.html) for
//! recording of HTTP `Dialog`s, currently via a basic integration of the
//! _hyper_ 0.11.x crate.
//!
//! _cli (default):_ The `barc` command line tool for viewing
//! (e.g. compressed) records and copying records across BARC files. If the
//! _client_ feature is enabled, than a `record` command is also provided for
//! live BARC recording from the network.
//!
//! _brotli (default):_ Brotli transfer/content decoding in the _client_, and
//! Brotli BARC record compression, via the native-rust _brotli_ crate. (Gzip,
//! via the _flate2_ crate, is standard.)
//!
//! For complete functionally, build or install with `--all-features`.

#[cfg(feature = "brotli")] extern crate brotli;
                           extern crate bytes;
#[macro_use]               extern crate failure;
                           extern crate flate2;
                           extern crate http;
                           extern crate httparse;
#[macro_use]               extern crate log;
                           extern crate memmap;
                           extern crate tempfile;

                           pub mod barc;
#[cfg(feature = "client")] pub mod client;

mod read_pos;
pub use read_pos::ReadPos;

use std::env;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use bytes::{Bytes, BytesMut, BufMut};
use memmap::Mmap;
use tempfile::tempfile_in;

/// The crate version string.
pub static VERSION: &str               = env!("CARGO_PKG_VERSION");

/// Error enumeration for `BodyImage` and `BodySink` types.  This may be
/// extended in the future so exhaustive matching is gently discouraged with
/// an unused variant.
#[derive(Fail, Debug)]
pub enum BodyError {
    /// Error for when `Tunables::max_body` length is exceeded.
    #[fail(display = "Body length of {}+ bytes exceeds Tunables::max_body",
           _0)]
    BodyTooLong(u64),

    /// IO error associated with file creation/writes `FsWrite`, reads
    /// `FsRead`, or memory mapping.
    #[fail(display = "{}", _0)]
    Io(#[cause] io::Error),

    /// Unused variant to both enable non-exhaustive matching and warn against
    /// exhaustive matching.
    #[fail(display = "The future!")]
    _FutureProof,
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
/// : Body in a memory mapped file, ready for random access read.
///
/// All states support concurrent reads. `BodyImage` is `Sync` and `Send`, and
/// supports low-cost shallow `Clone` via internal (atomic) reference
/// counting.
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
    MemMap(Arc<Mmap>),
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
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

    /// Create new instance based on an existing `Mmap` region.
    fn with_map(map: Mmap) -> BodyImage {
        let len = map.len() as u64;
        BodyImage {
            state: ImageState::MemMap(Arc::new(map)),
            len
        }
    }

    /// Create new instance from a single byte slice.
    pub fn from_slice<T>(bytes: T) -> BodyImage
        where T: Into<Bytes>
    {
        let mut bs = BodySink::with_ram_buffers(1);
        bs.save(bytes).expect("safe for Ram");
        bs.prepare().expect("safe for Ram")
    }

    /// Return true if in state `Ram`.
    pub fn is_ram(&self) -> bool {
        match self.state {
            ImageState::Ram(_) => true,
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

    /// Formerly, this rewound 'FsRead', but now is an unnecessary no-op. The
    /// `reader` method always returns a `Read` from the start of the
    /// `BodyImage` for all states.
    #[deprecated]
    pub fn prepare(&mut self) -> Result<&mut Self, BodyError> {
        Ok(self)
    }

    /// If `FsRead`, convert to `MemMap` by memory mapping the file. No-op for
    /// other states.
    pub fn mem_map(&mut self) -> Result<&mut Self, BodyError> {
        if let ImageState::FsRead(_) = self.state {
            assert!(self.len > 0);
            // Swap with empty, to move file out of FsRead
            if let ImageState::FsRead(file) = self.state.cut() {
                match unsafe { Mmap::map(&file) } {
                    Ok(map) => {
                        self.state = ImageState::MemMap(Arc::new(map));
                    }
                    Err(e) => {
                        // Restore FsRead on failure
                        self.state = ImageState::FsRead(file);
                        return Err(BodyError::from(e));
                    }
                }
            }
        }
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

    /// Clone self by shallow copy, returning a new `BodyImage`. This is
    /// currently infallible and deprecated in favor of `clone`.
    #[deprecated]
    pub fn try_clone(&self) -> Result<BodyImage, BodyError> {
        Ok(self.clone())
    }

    /// Return a new `BodyReader` enum over self. The enum provides a
    /// consistent `Read` reference, or can be destructured for access to
    /// the specific concrete types.
    pub fn reader(&self) -> BodyReader {
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
                BodyReader::File(ReadPos::new(f.clone(), self.len))
            }
            ImageState::MemMap(ref m) => {
                BodyReader::Contiguous(Cursor::new(m))
            }
        }
    }

    /// Given a `Read` object, a length estimate in bytes, and `Tunables` read
    /// and prepare a new `BodyImage`. `Tunables`, the estimate and actual
    /// length read will determine which buffering strategy is used. The
    /// length estimate provides a hint to use the file system from the start,
    /// which is more optimal than writing out accumulated `Ram` buffers
    /// later. If the length can't be estimated, use zero (0).
    pub fn read_from(r: &mut Read, len_estimate: u64, tune: &Tunables)
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
    pub fn write_to(&self, out: &mut Write) -> Result<u64, BodyError> {
        match self.state {
            ImageState::Ram(ref v) => {
                for b in v {
                    out.write_all(b)?;
                }
            }
            ImageState::MemMap(ref m) => {
                out.write_all(m)?;
            }
            ImageState::FsRead(ref f) => {
                let mut rp = ReadPos::new(f.clone(), self.len);
                io::copy(&mut rp, out)?;
            }
        }
        Ok(self.len)
    }
}

// Read all bytes from r, consume and write to a `BodySink` in state
// `FsWrite`, returning a final prepared `BodyImage`.
fn read_to_body_fs(r: &mut Read, mut body: BodySink, tune: &Tunables)
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
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
            ImageState::MemMap(ref m) => {
                f.debug_tuple("MemMap")
                    .field(m)
                    .finish()
            }
        }
    }
}

/// Provides a `Read` reference for a `BodyImage` in any state.
pub enum BodyReader<'a> {
    /// `Cursor` over a contiguous single RAM buffer, from `Ram` or
    /// `MemMap`. Also used for the empty case. `Cursor::into_inner` may be
    /// used for direct access to the memory byte slice.
    Contiguous(Cursor<&'a [u8]>),

    /// `GatheringReader` providing `Read` over 2 or more scattered RAM
    /// buffers.
    Scattered(GatheringReader<'a>),

    /// `ReadPos` providing instance independent `Read` and `Seek` for
    /// BodyImage `FsRead` state.
    File(ReadPos),
}

impl<'a> BodyReader<'a> {
    /// Return the `Read` reference.
    pub fn as_read(&mut self) -> &mut Read {
        match *self {
            BodyReader::Contiguous(ref mut cursor) => cursor,
            BodyReader::Scattered(ref mut gatherer) => gatherer,
            BodyReader::File(ref mut rpos) => rpos,
        }
    }
}

/// A specialized reader for `BodyImage` in `Ram`, presenting a continuous
/// (gathered) `Read` interface over N non-contiguous byte buffers.
pub struct GatheringReader<'a> {
    current: Cursor<&'a [u8]>,
    remainder: &'a [Bytes]
}

impl<'a> GatheringReader<'a> {
    pub fn new(buffers: &'a [Bytes]) -> Self {
        match buffers.split_first() {
            Some((b, remainder)) => {
                GatheringReader { current: Cursor::new(b), remainder }
            }
            None => {
                GatheringReader { current: Cursor::new(&[]), remainder: &[] }
            }
        }
    }

    fn pop(&mut self) -> bool {
        match self.remainder.split_first() {
            Some((c, rem)) => {
                self.current = Cursor::new(c);
                self.remainder = rem;
                true
            }
            None => false
        }
    }
}

impl<'a> Read for GatheringReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.current.read(buf)?;
        if n == 0 && !buf.is_empty() && self.pop() {
            return self.read(buf); // recurse
        }
        Ok(n)
    }
}

/// Saved extract from an HTTP request.
#[derive(Clone, Debug)]
struct Prolog {
    method:       http::Method,
    url:          http::Uri,
    req_headers:  http::HeaderMap,
    req_body:     BodyImage,
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
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
/// Note that several important getter methods for `Dialog` are found in trait
/// implementations [`RequestRecorded`](#impl-RequestRecorded) and
/// [`Recorded`](#impl-Recorded).
#[derive(Clone, Debug)]
pub struct Dialog {
    prolog:       Prolog,
    version:      http::Version,
    status:       http::StatusCode,
    res_decoded:  Vec<Encoding>,
    res_headers:  http::HeaderMap,
    res_body:     BodyImage,
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
    /// The HTTP method (verb), e.g. `GET`, `POST`, etc.
    pub fn method(&self)      -> &http::Method         { &self.prolog.method }

    /// The complete URL as used in the request.
    pub fn url(&self)         -> &http::Uri            { &self.prolog.url }

    /// A mutable reference to the request body. This is primarly provided
    /// to allow state mutating operations such as `BodyImage::mem_map`.
    pub fn req_body_mut(&mut self) -> &mut BodyImage {
        &mut self.prolog.req_body
    }

    /// The response status code.
    pub fn res_status(&self)  -> http::StatusCode      { self.status }

    /// The response HTTP version.
    pub fn res_version(&self) -> http::Version         { self.version }

    /// A list of encodings that were removed (decoded) to provide this
    /// representation of the response body (`res_body`). May be empty.
    pub fn res_decoded(&self) -> &Vec<Encoding>        { &self.res_decoded }

    /// A mutable reference to the response body. This is primarly provided
    /// to allow state mutating operations such as `BodyImage::mem_map`.
    pub fn res_body_mut(&mut self) -> &mut BodyImage   { &mut self.res_body }

    /// Clone self and return a new `Dialog`. This is currently infallible and
    /// deprecated in favor of `clone`.
    #[deprecated]
    pub fn try_clone(&self) -> Result<Dialog, BodyError> {
        Ok(self.clone())
    }
}

impl RequestRecorded for Dialog {
    fn req_headers(&self) -> &http::HeaderMap      { &self.prolog.req_headers }
    fn req_body(&self)    -> &BodyImage            { &self.prolog.req_body }
}

impl Recorded for Dialog {
    fn res_headers(&self) -> &http::HeaderMap      { &self.res_headers }
    fn res_body(&self)    -> &BodyImage            { &self.res_body }
}

/// A collection of size limits and performance tuning constants. Setters are
/// available via the `Tuner` class.
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
            temp_dir:      env::temp_dir(),
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

    /// Return the buffer size in bytes to use when buffering for output in
    /// RAM. Default: 8 KiB.
    pub fn buffer_size_ram(&self) -> usize {
        self.buffer_size_ram
    }

    /// Return the buffer size in bytes to use when buffering for output to
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

    /// Set the buffer size in bytes to use when buffering for output in RAM.
    pub fn set_buffer_size_ram(&mut self, size: usize) -> &mut Tuner {
        assert!(size > 0, "buffer_size_ram must be greater than zero");
        self.template.buffer_size_ram = size;
        self
    }

    /// Set the buffer size in bytes to use when buffering for output to the
    /// file-system.
    pub fn set_buffer_size_fs(&mut self, size: usize) -> &mut Tuner {
        assert!(size > 0, "buffer_size_fs must be greater than zero");
        self.template.buffer_size_fs = size;
        self
    }

    /// Set the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _deflate_ compression algorithm.
    pub fn set_size_estimate_deflate(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_deflate must be >= 1" );
        self.template.size_estimate_deflate = multiple;
        self
    }

    /// Set the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _gzip_ compression algorithm.
    pub fn set_size_estimate_gzip(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_gzip must be >= 1" );
        self.template.size_estimate_gzip = multiple;
        self
    }

    /// Set the size estimate, as an integer multiple of the encoded buffer
    /// size, for the _Brotli_ compression algorithm.
    pub fn set_size_estimate_brotli(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_brotli must be >= 1" );
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

    /// Finish building, asserting any remaining invariants, and return a new
    /// `Tunables` instance.
    pub fn finish(&self) -> Tunables {
        let t = self.template.clone();
        assert!(t.max_body_ram <= t.max_body,
                "max_body_ram can't be greater than max_body");
        t
    }
}

impl Default for Tuner {
    fn default() -> Self { Tuner::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_send<T: Send>() -> bool { true }
    fn is_sync<T: Sync>() -> bool { true }

    #[test]
    fn test_send_sync() {
        assert!(is_send::<BodyImage>());
        assert!(is_sync::<BodyImage>());

        assert!(is_send::<Tunables>());
        assert!(is_sync::<Tunables>());

        assert!(is_send::<Dialog>());
        assert!(is_sync::<Dialog>());

        assert!(is_send::<BodyReader>());
        assert!(is_sync::<BodyReader>());
    }

    #[test]
    fn test_body_empty_read() {
        let body = BodyImage::empty();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = Vec::new();
        br.read_to_end(&mut obuf).unwrap();
        assert!(obuf.is_empty());
    }

    #[test]
    fn test_body_contiguous_read() {
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
    fn test_body_scattered_read() {
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
    fn test_body_scattered_read_clone() {
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
    fn test_body_scattered_gather() {
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
    fn test_body_read_from() {
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
    fn test_body_read_from_too_long() {
        let tune = Tuner::new()
            .set_max_body_ram(6)
            .set_max_body(6)
            .finish();
        let salutation = b"hello world";
        let mut src = Cursor::new(salutation);
        if let Err(e) = BodyImage::read_from(&mut src, 0, &tune) {
            if let BodyError::BodyTooLong(l) = e {
                assert_eq!(l, salutation.len() as u64)
            }
            else {
                panic!("Other error: {}", e);
            }
        }
        else {
            panic!("Read from, too long, success!?");
        }
    }

    #[test]
    fn test_body_fs_read() {
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
    fn test_body_fs_back_read() {
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
    fn test_body_w_back_w_read() {
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
    fn test_body_fs_back_fail() {
        let tune = Tuner::new().set_temp_dir("./no-existe/").finish();
        let mut body = BodySink::with_ram_buffers(2);
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        if let Err(_) = body.write_back(tune.temp_dir()) {
            assert!(body.is_ram());
            assert_eq!(body.len(), 0);
        } else {
            panic!("write_back with bogus dir success?!");
        }
    }

    #[test]
    fn test_body_fs_map_read() {
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
    fn test_body_fs_back_read_clone() {
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

    #[test]
    fn test_body_fs_clone_shared() {
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
    fn test_body_fs_empty() {
        let tune = Tunables::new();
        let body = BodySink::with_fs(tune.temp_dir()).unwrap();
        let mut body = body.prepare().unwrap();
        assert!(body.is_empty());

        body.mem_map().unwrap();
        assert!(body.is_empty());
    }
}
