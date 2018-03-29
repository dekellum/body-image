#![feature(external_doc)]
#![doc(include = "../README.md")]

extern crate bytes;
#[macro_use] extern crate failure;
extern crate futures;
extern crate http;
extern crate httparse;
#[cfg(feature = "client")] extern crate hyper;
#[cfg(feature = "client")] extern crate hyper_tls;
#[macro_use] extern crate log;
extern crate memmap;
extern crate tempfile;
extern crate tokio_core;
#[cfg(feature = "brotli")] extern crate brotli;
extern crate flate2;

pub mod barc;
#[cfg(feature = "client")] pub mod client;

/// Alias for failure crate `failure::Error`
use failure::Error as FlError;
// FIXME: Use at least while prototyping. Could switch to an error enum
// for clear separation between hyper::Error and application errors.

use std::env;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom, Write};
use std::mem;
use std::path::{Path, PathBuf};
use bytes::{Bytes, BytesMut, BufMut};
use memmap::Mmap;
use tempfile::tempfile_in;

/// A logical buffer of bytes, which may or may not be RAM resident.
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
/// : Body in a (temporary) file, ready for position based, single access,
///   sequential read.
///
/// `MemMap`
/// : Body in a memory mapped file, ready for efficient and potentially
///   concurrent and random access reading.
///
#[derive(Debug)]
pub struct BodyImage {
    state: ImageState,
    len: u64
}

// Internal state enum for BodyImage
enum ImageState {
    Ram(Vec<Bytes>),
    FsRead(File),
    MemMap(Mapped),
}

/// A logical buffer of bytes, which may or may not be RAM resident, in the
/// process of being written. This is the write-side corollary to `BodyImage`.
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
///
#[derive(Debug)]
pub struct BodySink {
    state: SinkState,
    len: u64
}

enum SinkState {
    Ram(Vec<Bytes>),
    FsWrite(File),
}

/// A memory map handle with its underlying `File` as a RAII guard.
#[derive(Debug)]
pub struct Mapped {
    map: Mmap,
    file: File,
    // This ordering has `munmap` called before close on destruction,
    // which seems best, though it may not actually be a requirement
    // to keep the File open, at least on Linux.
}

impl SinkState {
    // Swap self with empty `Ram` and return an owned self.
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
    pub fn with_fs<P>(dir: P) -> Result<BodySink, FlError>
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
    pub fn save<T>(&mut self, buf: T) -> Result<(), FlError>
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
    pub fn write_all<T>(&mut self, buf: T) -> Result<(), FlError>
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
    pub fn write_back<P>(&mut self, dir: P) -> Result<&mut Self, FlError>
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
    pub fn prepare(self) -> Result<BodyImage, FlError> {
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
                        state: ImageState::FsRead(f),
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

    // Swap self with empty `Ram` and return an owned self
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

    /// Create new instance based on an existing `Mapped` file.
    fn with_map(mapped: Mapped) -> BodyImage {
        let len = mapped.map.len() as u64;
        BodyImage {
            state: ImageState::MemMap(mapped),
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

    /// Prepare for re-reading. If `FsRead`, seeks to beginning of file.
    /// No-op for other states.
    pub fn prepare(&mut self) -> Result<&mut Self, FlError> {
        if let ImageState::FsRead(ref mut f) = self.state {
            f.seek(SeekFrom::Start(0))?;
        }
        Ok(self)
    }

    /// If `FsRead`, convert to `MemMap` by memory mapping the file.
    /// No-op for other states.
    pub fn mem_map(&mut self) -> Result<&mut Self, FlError> {
        if let ImageState::FsRead(_) = self.state {
            assert!(self.len > 0);
            // Swap with empty, to move file out of FsRead
            if let ImageState::FsRead(file) = self.state.cut() {
                match unsafe { Mmap::map(&file) } {
                    Ok(map) => {
                        self.state = ImageState::MemMap(
                            Mapped { map, file }
                        );
                    }
                    Err(e) => {
                        // Restore FsRead on failure
                        self.state = ImageState::FsRead(file);
                        return Err(FlError::from(e));
                    }
                }
            }
        }
        Ok(self)
    }

    /// If `MemMap`, unmap, converting back to the original `FsRead`.
    /// No-op for other states.
    pub fn mem_unmap(&mut self) -> &mut Self {
        if let ImageState::MemMap(_) = self.state {
            if let ImageState::MemMap(m) = self.state.cut() {
                self.state = ImageState::FsRead(m.file);
            }
        }
        self
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
                BodyReader::File(f)
            }
            ImageState::MemMap(ref m) => {
                BodyReader::Contiguous(Cursor::new(&m.map))
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
        -> Result<BodyImage, FlError>
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
                debug!("Decoded inner buf len {}", len);
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
                bail!("Body is too long: {}+", size);
            }
            if size > tune.max_body_ram() {
                body.write_back(tune.temp_dir())?;
                debug!("Write (Fs) decoded buf len {}", len);
                body.write_all(&buf)?;
                return read_to_body_fs(r, body, tune)
            }
            debug!("Saved (Ram) decoded buf len {}", len);
            body.save(buf.freeze())?;
        }
        let body = body.prepare()?;
        Ok(body)
    }

    /// Write self to `out` and return length. If in state `FsRead`, a
    /// temporary memory map will be made in order to write without
    /// mutating self, using or changing the file position.
    pub fn write_to(&self, out: &mut Write) -> Result<u64, FlError> {
        match self.state {
            ImageState::Ram(ref v) => {
                for b in v {
                    out.write_all(b)?;
                }
            }
            ImageState::MemMap(ref m) => {
                let map = &m.map;
                out.write_all(map)?;
            }
            ImageState::FsRead(ref f) => {
                assert!(self.len > 0);
                let tmap = unsafe { Mmap::map(f) }?;
                let map = &tmap;
                out.write_all(map)?;
            }
        }
        Ok(self.len)
    }
}

// Read all bytes from r, consume and write to a `BodySink` in state
// `FsWrite`, returning a final prepared `BodyImage`.
fn read_to_body_fs(r: &mut Read, mut body: BodySink, tune: &Tunables)
    -> Result<BodyImage, FlError>
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
            bail!("Body is too long: {}+", size);
        }
        debug!("Write (Fs) decoded buf len {}", len);
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

    /// File reference providing `Read`, from BodyImage `FsRead` state.
    File(&'a File),
}

impl<'a> BodyReader<'a> {
    /// Return the `Read` reference.
    pub fn as_read(&mut self) -> &mut Read {
        match *self {
            BodyReader::Contiguous(ref mut cursor) => cursor,
            BodyReader::Scattered(ref mut gatherer) => gatherer,
            BodyReader::File(ref mut file) => file,
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
#[derive(Debug)]
struct Prolog {
    method:       http::Method,
    url:          http::Uri,
    req_headers:  http::HeaderMap,
    req_body:     BodyImage,
}

/// An HTTP request and response recording.
#[derive(Debug)]
pub struct Dialog {
    meta:         http::HeaderMap,
    prolog:       Prolog,
    version:      http::Version,
    status:       http::StatusCode,
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

/// Access by reference for HTTP request and response recording types.
pub trait Recorded: RequestRecorded {
    /// Map of _meta_-headers for values which are not strictly part of the
    /// HTTP request or response headers.
    fn meta(&self)        -> &http::HeaderMap;

    /// Map of HTTP response headers.
    fn res_headers(&self) -> &http::HeaderMap;

    /// Response body which may or may not be RAM resident.
    fn res_body(&self)    -> &BodyImage;
}

impl RequestRecorded for Dialog {
    fn req_headers(&self) -> &http::HeaderMap      { &self.prolog.req_headers }
    fn req_body(&self)    -> &BodyImage            { &self.prolog.req_body }
}

impl Recorded for Dialog {
    fn meta(&self)        -> &http::HeaderMap      { &self.meta }
    fn res_headers(&self) -> &http::HeaderMap      { &self.res_headers }
    fn res_body(&self)    -> &BodyImage            { &self.res_body }
}

/// Meta `HeaderName` for the complete URL used in the request.
pub static META_URL: &[u8]             = b"url";

/// Meta `HeaderName` for the HTTP method used in the request, e.g. "GET",
/// "POST", etc.
pub static META_METHOD: &[u8]          = b"method";

/// Meta `HeaderName` for the response version, e.g. "HTTP/1.1", "HTTP/2.0",
/// etc.
pub static META_RES_VERSION: &[u8]     = b"response-version";

/// Meta `HeaderName` for the response numeric status code, SPACE, and then a
/// standardized _reason phrase_, e.g. "200 OK". The later is intended only
/// for human readers.
pub static META_RES_STATUS: &[u8]      = b"response-status";

/// Meta `HeaderName` for a list of content or transfer encodings decoded for
/// the current response body. The value is in HTTP content-encoding header
/// format, e.g. "chunked, gzip".
pub static META_RES_DECODED: &[u8]     = b"response-decoded";

impl Dialog {
    /// If the request body is in state `FsRead`, convert to `MemMap` via
    /// `BodyImage::mem_map` and return reference to the body.
    pub fn mem_map_req_body(&mut self) -> Result<&BodyImage, FlError> {
        let b = self.prolog.req_body.mem_map()?;
        Ok(b)
    }

    /// If the response body is in state `FsRead`, convert to `MemMap` via
    /// `BodyImage::mem_map` and return reference to the body.
    pub fn mem_map_res_body(&mut self) -> Result<&BodyImage, FlError> {
        let b = self.res_body.mem_map()?;
        Ok(b)
    }
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
    fn test_body_fs_map_unmap_read() {
        let tune = Tunables::new();
        let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
        body.write_all("hello").unwrap();
        body.write_all(" ").unwrap();
        body.write_all("world").unwrap();
        let mut body = body.prepare().unwrap();
        body.mem_map().unwrap();
        body.mem_unmap();
        let mut body_reader = body.reader();
        let br = body_reader.as_read();
        let mut obuf = String::new();
        br.read_to_string(&mut obuf).unwrap();
        assert_eq!("hello world", &obuf[..]);
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
