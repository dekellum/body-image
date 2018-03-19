#[macro_use] extern crate failure;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate hyper_tls;
extern crate memmap;
extern crate tempfile;
extern crate tokio_core;

pub mod barc;
pub mod compress;

/// Alias for failure crate `failure::Error`
use failure::Error as FlError;
// FIXME: Use atleast while prototyping. Could switch to an error enum
// for clear separation between hyper::Error and application errors.

use std::fmt;
use std::fs::File;
use std::io;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use futures::{Future, Stream};
use futures::future::err as futerr;
use futures::future::result as futres;
use hyper::{Chunk, Client};
use hyper::client::compat::CompatFutureResponse;
use hyper::header::{ContentLength, Header, Raw};
use memmap::Mmap;
use tempfile::tempfile;
use tokio_core::reactor::Core;

/// Alias for `hyper::Body`.
pub use hyper::Body as HyBody;

/// The HTTP request (with body) type (as of hyper 0.11.x.)
pub type HyRequest = http::Request<HyBody>;

/// An HTTP request or response body as bytes, which may or may not be
/// RAM resident.
///
/// A `BodyImage` is always in one of the following states, as a
/// buffering strategy:
///
/// `Ram`
/// : A vector of one or more byte buffers in Random Access Memory,
///   for read or write. This state is also used to represent an empty
///   body (without allocation).
///
/// `FsWrite`
/// : Body in process of being written to a (temporary) file.
///
/// `FsRead`
/// : Body in a (temporary) file, ready for position based, single
///   access, sequential read.
///
/// `MemMap`
/// : Body in a memory mapped file, ready for efficient and
///   potentially concurrent and repeated reading.
///
#[derive(Debug)]
pub struct BodyImage {
    state: BodyState,
    len: u64
}

// Internal state enum for BodyImage
enum BodyState {
    Ram(Vec<Chunk>),
    FsWrite(File),
    FsRead(File),
    MemMap(Mapped),
}

/// A memory map handle with its underlying `File` as a RAII guard.
#[derive(Debug)]
pub struct Mapped {
    map: Mmap,
    _file: File,
    // This ordering has munmap called before close on destruction,
    // which seems best, though it may not actually be a requirement
    // to keep the File open, at least on Unix.
}

impl BodyImage {
    /// Create new empty instance, which does not pre-allocate. The state
    /// is `Ram` with a zero-capacity vector.
    pub fn empty() -> BodyImage {
        BodyImage::with_chunks_capacity(0)
    }

    /// Create a new `Ram` instance by pre-allocating a vector of
    /// chunks based on the given size estimate in bytes. With a
    /// size_estimate of 0, this is the same as `empty`.
    pub fn with_ram(size_estimate: u64) -> BodyImage {
        if size_estimate == 0 {
            BodyImage::empty()
        } else {
            // Estimate capacity based on observed 8 KiB chunks
            let chunks = (size_estimate / 0x2000 + 1) as usize;
            BodyImage::with_chunks_capacity(chunks)
        }
    }

    /// Create a new `Ram` instance by pre-allocating a vector of the
    /// specified capacity expressed as number chunks.
    pub fn with_chunks_capacity(capacity: usize) -> BodyImage {
        BodyImage {
            state: BodyState::Ram(Vec::with_capacity(capacity)),
            len: 0
        }
    }

    /// Create a new instance from a new temporary file, in state
    /// `FsWrite`.
    pub fn with_fs() -> Result<BodyImage, FlError> {
        // FIXME: Add control for setting dir for the tempfile,
        // possibly via Tunables.
        let f = tempfile()?;
        Ok(BodyImage {
            state: BodyState::FsWrite(f),
            len: 0
        })
    }

    /// Create new instance based on an existing `Mapped` file.
    fn with_map(mapped: Mapped) -> BodyImage {
        let len = mapped.map.len() as u64;
        BodyImage {
            state: BodyState::MemMap(mapped),
            len
        }
    }

    /// Return true if in state `Ram`.
    pub fn is_ram(&self) -> bool {
        match self.state {
            BodyState::Ram(_) => true,
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

    /// Save `Chunk` by appending to `Ram` or writing to `FsWrite`
    /// file. Panics if in some other state.
    pub fn save(&mut self, chunk: Chunk) -> Result<(), FlError> {
        let len = chunk.len() as u64;
        match self.state {
            BodyState::Ram(ref mut v) => {
                v.push(chunk);
            }
            BodyState::FsWrite(ref mut f) => {
                f.write_all(&chunk)?;
            }
            _ => {
                panic!("Invalid state for save(): {:?}", self);
            }
        }
        self.len += len;
        Ok(())
    }

    /// Write all bytes of slice by reference to self, which must be
    /// in state `FsWrite`. This can be an optimization over `save`
    /// which requires an owned Chunk. Panics if in some other state.
    pub fn write_all(&mut self, buf: &[u8]) -> Result<(), FlError> {
        if let BodyState::FsWrite(ref mut f) = self.state {
            f.write_all(buf)?;
            self.len += buf.len() as u64;
            Ok(())
        }
        else {
            panic!("Invalid state for write_all(): {:?}", self);
        }
    }

    /// Convert from `Ram` to `FsWrite`, writing all existing chunks
    /// to a new temporary file. Panics if in some other state.
    pub fn write_back(&mut self) -> Result<(), FlError> {
        self.state = if let BodyState::Ram(ref v) = self.state {
            let mut f = tempfile()?;
            for c in v {
                f.write_all(c)?;
            }
            BodyState::FsWrite(f)
        } else {
            panic!("Invalid state for write_back(): {:?}", self);
        };
        Ok(())
    }

    /// Prepare for (re-)reading. Consumes and converts from `FsWrite`
    /// to `FsRead`.  Seeks to beginning of file for either of these
    /// states. No-op for other states.
    pub fn prepare(mut self) -> Result<Self, FlError> {
        match self.state {
            BodyState::FsWrite(mut f) => {
                f.flush()?;
                f.seek(SeekFrom::Start(0))?;
                self.state = BodyState::FsRead(f);
            }
            BodyState::FsRead(ref mut f) => {
                f.seek(SeekFrom::Start(0))?;
            }
            _ => {}
        }
        Ok(self)
    }

    /// Consumes self state `FsRead` and returns `MemMap` by memory
    /// mapping the file.  Panics if self is in some other state.
    pub fn map(mut self) -> Result<Self, FlError> {
        if let BodyState::FsRead(file) = self.state {
            assert!(self.len > 0);
            let map = unsafe { Mmap::map(&file)? };
            self.state = BodyState::MemMap(Mapped { map, _file: file });
            Ok(self)
        } else {
            panic!("Invalid state for map(): {:?}", self);
        }
    }

    /// Return a new `BodyReader` over self. Panics if in state
    /// `FsWrite`. Avoid this by using `prepare` first.
    pub fn reader(&self) -> BodyReader {
        match self.state {
            BodyState::Ram(ref v) =>
                BodyReader::FromRam(ChunksReader::new(v)),
            BodyState::FsWrite(_) =>
                panic!("Invalid state BodyState::FsWrite::reader()"),
            BodyState::FsRead(ref f) =>
                BodyReader::FromFs(f),
            BodyState::MemMap(ref m) =>
                BodyReader::FromMemMap(Cursor::new(&m.map)),
        }
    }

    /// Specialized and efficient `Write::write_all` for states `Ram`
    /// and `MemMap` which can be read without mutating. Panics if in
    /// some other state.
    pub fn write_to(&self, out: &mut Write) -> Result<u64, FlError> {
        match self.state {
            BodyState::Ram(ref v) => {
                let mut size: u64 = 0;
                for c in v {
                    let b = &c;
                    out.write_all(b)?;
                    size += b.len() as u64;
                }
                Ok(size)
            }
            BodyState::MemMap(ref m) => {
                let map = &m.map;
                out.write_all(map)?;
                Ok(map.len() as u64)
            }
            _ => {
                panic!("Invalid state for write_to(): {:?}", self);
            }

        }
    }
}

impl Default for BodyImage {
    fn default() -> BodyImage { BodyImage::empty() }
}

impl fmt::Debug for BodyState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BodyState::Ram(ref v) => {
                // Avoids showing all chunks as u8 lists
                f.debug_struct("Ram(Vec<Chunk>)")
                    .field("capacity", &v.capacity())
                    .field("len", &v.len())
                    .finish()
            }
            BodyState::FsRead(ref file) => {
                f.debug_tuple("FsRead")
                    .field(file)
                    .finish()
            }
            BodyState::FsWrite(ref file) => {
                f.debug_tuple("FsWrite")
                    .field(file)
                    .finish()
            }
            BodyState::MemMap(ref m) => {
                f.debug_tuple("MemMap")
                    .field(m)
                    .finish()
            }
        }
    }
}

/// Provides a `Read` reference for a `BodyImage` in various states.
pub enum BodyReader<'a> {
    FromRam(ChunksReader<'a>),
    FromFs(&'a File),
    FromMemMap(Cursor<&'a [u8]>),
}

impl<'a> BodyReader<'a> {
    /// Return a Read reference for self.
    pub fn as_read(&mut self) -> &mut Read {
        match *self {
            BodyReader::FromRam(ref mut cr) => cr,
            BodyReader::FromFs(ref mut f) => f,
            BodyReader::FromMemMap(ref mut cur) => cur,
        }
    }
}

/// A specialized chaining reader for `BodyImage` in state `Ram`.
pub struct ChunksReader<'a> {
    current: Cursor<&'a [u8]>,
    remainder: &'a [Chunk]
}

impl<'a> ChunksReader<'a> {
    pub fn new(chunks: &'a [Chunk]) -> Self {
        match chunks.split_first() {
            Some((c, remainder)) => {
                ChunksReader { current: Cursor::new(c), remainder }
            }
            None => {
                ChunksReader { current: Cursor::new(&[]), remainder: &[] }
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

impl<'a> Read for ChunksReader<'a> {
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

/// An `http::Request` and recording.
#[derive(Debug)]
pub struct RequestRecord {
    request:      HyRequest,
    prolog:       Prolog,
}

/// Temporary `http::Response` wrapper, with preserved request
/// recording.
#[derive(Debug)]
struct Monolog {
    prolog:       Prolog,
    response:     http::Response<HyBody>,
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

    /// Request body (e.g for HTTP POST, etc.) which may or may not be
    /// RAM resident.
    fn req_body(&self)    -> &BodyImage;
}

/// Access by reference for HTTP request and response recording types.
pub trait Recorded: RequestRecorded {
    /// Map of _meta_-headers for values which are not strictly part
    /// of the HTTP request or response headers.
    fn meta(&self)        -> &http::HeaderMap;

    /// Map of HTTP response headers.
    fn res_headers(&self) -> &http::HeaderMap;

    /// Response body which may or may not be RAM resident.
    fn res_body(&self)    -> &BodyImage;
}

impl RequestRecord {
    /// Return the HTTP request.
    pub fn request(&self) -> &HyRequest            { &self.request }
}

impl RequestRecorded for RequestRecord {
    fn req_headers(&self) -> &http::HeaderMap      { &self.prolog.req_headers }
    fn req_body(&self)    -> &BodyImage            { &self.prolog.req_body }
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

/// Meta `HeaderName` for the HTTP method used in the request,
/// e.g. "GET", "POST", etc.
pub static META_METHOD: &[u8]          = b"method";

/// Meta `HeaderName` for the response version, e.g. "HTTP/1.1",
/// "HTTP/2.0", etc.
pub static META_RES_VERSION: &[u8]     = b"response-version";

/// Meta `HeaderName` for the response numeric status code, SPACE, and
/// then a standardized _reason phrase_, e.g. "200 OK". The later is
/// intended only for human readers.
pub static META_RES_STATUS: &[u8]      = b"response-status";

/// Meta `HeaderName` for a list of content or transfer encodings
/// decoded for the current response body. The value is in HTTP
/// content-encoding header format, e.g. "chunked, gzip".
pub static META_RES_DECODED: &[u8]     = b"response-decoded";

impl Dialog {
    /// Prepare the response body for reading and generate meta
    /// headers.
    fn prepare(mut self) -> Result<Self, FlError> {
        self.res_body = self.res_body.prepare()?;
        self.meta = self.derive_meta()?;
        Ok(self)
    }

    fn derive_meta(&self) -> Result<http::HeaderMap, FlError> {
        let mut hs = http::HeaderMap::with_capacity(6);
        use http::header::HeaderName;

        hs.append(HeaderName::from_lowercase(META_URL).unwrap(),
                  self.prolog.url.to_string().parse()?);
        hs.append(HeaderName::from_lowercase(META_METHOD).unwrap(),
                  self.prolog.method.to_string().parse()?);

        // FIXME: This relies on the debug format of version,  e.g. "HTTP/1.1"
        // which might not be stable, but http::Version doesn't offer an enum
        // to match on, only constants.
        let v = format!("{:?}", self.version);
        hs.append(HeaderName::from_lowercase(META_RES_VERSION).unwrap(),
                  v.parse()?);

        hs.append(HeaderName::from_lowercase(META_RES_STATUS).unwrap(),
                  self.status.to_string().parse()?);
        Ok(hs)
    }

    /// If the response body is `FsRead`, convert it to `MemMap` via
    /// `BodyImage::map`, else no-op.
    pub fn map_if_fs(mut self) -> Result<Self, FlError> {
        if let BodyState::FsRead(_) = self.res_body.state {
            self.res_body = self.res_body.map()?;
        }
        Ok(self)
    }
}

/// A collection of size limits and performance tuning
/// constants. Setters are available via the `Tuner` class.
#[derive(Debug, Clone, Copy)]
pub struct Tunables {
    max_body_ram:            u64,
    max_body:                u64,
    decode_buffer_ram:       usize,
    decode_buffer_fs:        usize,
    size_estimate_deflate:   u16,
    size_estimate_gzip:      u16,
}

impl Tunables {
    /// Construct with default values.
    pub fn new() -> Tunables {
        Tunables {
            max_body_ram:       192 * 1024,
            max_body:   1024 * 1024 * 1024,
            decode_buffer_ram:    8 * 1024,
            decode_buffer_fs:    64 * 1024,
            size_estimate_deflate:       4,
            size_estimate_gzip:          5,
        }
    }

    /// Return the maximum body size in bytes allowed in RAM,
    /// e.g. before writing to a temporary file, or memory mapping
    /// instead of direct, bulk read. Default: 192 KiB.
    pub fn max_body_ram(&self) -> u64 {
        self.max_body_ram
    }

    /// Return the maximum body size in bytes allowed in any form (RAM
    /// or file). Default: 1 GiB.
    pub fn max_body(&self) -> u64 {
        self.max_body
    }

    /// Return the buffer size in bytes to use for decoding, with
    /// output to RAM. Default: 8 KiB.
    pub fn decode_buffer_ram(&self) -> usize {
        self.decode_buffer_ram
    }

    /// Return the buffer size in bytes to use for decoding, with
    /// output to a file. Default: 64 KiB.
    pub fn decode_buffer_fs(&self) -> usize {
        self.decode_buffer_fs
    }

    /// Return the size estimate, as an integer multiple of the
    /// encoded buffer size, for the _gzip_ compression algorithm.
    /// Default: 5.
    pub fn size_estimate_gzip(&self) -> u16 {
        self.size_estimate_gzip
    }

    /// Return the size estimate, as an integer multiple of the
    /// encoded buffer size, for the _deflate_ compression algorithm.
    /// Default: 4.
    pub fn size_estimate_deflate(&self) -> u16 {
        self.size_estimate_deflate
    }
}

impl Default for Tunables {
    fn default() -> Self { Tunables::new() }
}

/// A builder for `Tunables`.  Invariants are asserted in the various
/// setters and `finish`.
#[derive(Clone, Copy)]
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
    /// file). This must be larger than `max_body_ram`, as asserted on
    /// `finish`.
    pub fn set_max_body(&mut self, size: u64) -> &mut Tuner {
        self.template.max_body = size;
        self
    }

    /// Set the buffer size in bytes to use for decoding, with output
    /// to RAM.
    pub fn set_decode_buffer_ram(&mut self, size: usize) -> &mut Tuner {
        assert!(size > 0, "decode_buffer_ram must be greater than zero");
        self.template.decode_buffer_ram = size;
        self
    }

    /// Set the buffer size in bytes to use for decoding, with output
    /// to a file.
    pub fn set_decode_buffer_fs(&mut self, size: usize) -> &mut Tuner {
        assert!(size > 0, "decode_buffer_fs must be greater than zero");
        self.template.decode_buffer_fs = size;
        self
    }

    /// Set the size estimate, as an integer multiple of the
    /// encoded buffer size, for the _gzip_ compression algorithm.
    pub fn set_size_estimate_gzip(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_gzip must be >= 1" );
        self.template.size_estimate_gzip = multiple;
        self
    }

    /// Set the size estimate, as an integer multiple of the
    /// encoded buffer size, for the _deflate_ compression algorithm.
    pub fn set_size_estimate_deflate(&mut self, multiple: u16) -> &mut Tuner {
        assert!(multiple > 0, "size_estimate_deflate must be >= 1" );
        self.template.size_estimate_deflate = multiple;
        self
    }

    /// Finish building, asserting any remaining invariants, and
    /// return a new `Tunables` instance.
    pub fn finish(&self) -> Tunables {
        let t = self.template;
        assert!(t.max_body_ram <= t.max_body,
                "max_body_ram can't be greater than max_body");
        t
    }
}

impl Default for Tuner {
    fn default() -> Self { Tuner::new() }
}

/// Run an HTTP request to completion, returning the full `Dialog`.
pub fn fetch(rr: RequestRecord, tune: &Tunables) -> Result<Dialog, FlError> {
    // FIXME: State of the Core (v Reactor), incl. construction,
    // use from multiple threads is under flux:
    // https://tokio.rs/blog/2018-02-tokio-reform-shipped/
    //
    // But hyper, as of 0.11.18 still depends on tokio-core, io,
    // service:
    // https://crates.io/crates/hyper
    let mut core = Core::new()?;
    let client = Client::configure()
        .connector(hyper_tls::HttpsConnector::new(4, &core.handle())?)
        // FIXME: threads ------------------------^
        .build(&core.handle());

    // FIXME: What about Timeouts? Appears to also be under flux:
    // https://github.com/hyperium/hyper/issues/1234
    // https://hyper.rs/guides/client/timeout/

    let prolog = rr.prolog;

    let fr: CompatFutureResponse = client.request_compat(rr.request);

    let work = fr
        .map(|response| Monolog { prolog, response } )
        .map_err(FlError::from)
        .and_then(|monolog| resp_future(monolog, *tune))
        .and_then(|dialog| futres(dialog.prepare()));

    // Run until completion
    core.run(work)
        .map_err(FlError::from)
}

fn resp_future(monolog: Monolog, tune: Tunables)
    -> Box<Future<Item=Dialog, Error=FlError> + Send>
{
    let (resp_parts, body) = monolog.response.into_parts();

    // Result<BodyImage> based on CONTENT_LENGTH header.
    let bi = match resp_parts.headers.get(http::header::CONTENT_LENGTH) {
        Some(v) => check_length(v, tune.max_body()).and_then(|cl| {
            if cl > tune.max_body_ram() {
                BodyImage::with_fs()
            } else {
                Ok(BodyImage::with_ram(cl))
            }
        }),
        None => Ok(BodyImage::with_ram(tune.max_body_ram()))
    };

    // Unwrap BodyImage, returning any error as Future
    let bi = match bi {
        Ok(b) => b,
        Err(e) => { return Box::new(futerr(e)); }
    };

    let dialog = Dialog {
        meta:        http::HeaderMap::with_capacity(0),
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        res_body:    bi,
    };

    let s = body
        .map_err(FlError::from)
        .fold(dialog, move |mut dialog, chunk| {
            let new_len = dialog.res_body.len() + (chunk.len() as u64);
            if new_len > tune.max_body() {
                bail!("Response stream too long: {}+", new_len);
            } else {
                if dialog.res_body.is_ram() && new_len > tune.max_body_ram() {
                    dialog.res_body.write_back()?;
                }
                println!("to save chunk (len: {})", chunk.len());
                dialog.res_body
                    .save(chunk)
                    .and(Ok(dialog))
            }
        });
    Box::new(s)
}

fn check_length(v: &http::header::HeaderValue, max: u64)
    -> Result<u64, FlError>
{
    let l = *ContentLength::parse_header(&Raw::from(v.as_bytes()))?;
    if l > max {
        bail!("Response Content-Length too long: {}", l);
    }
    Ok(l)
}

/// Extension trait for `http::request::Builder`, to enable recording
/// key portions of the request for the final `Dialog`.
///
/// In particular any request body (e.g. POST, PUT) needs to be cloned
/// in advance of finishing the request, because `hyper::Body` isn't
/// `Clone`.
///
/// _Limitation_: Currently only a contiguous RAM buffer (implementing
/// `Into<Chunk>`and `Clone`) is supported as the request body.
pub trait RequestRecordable {
    // Short-hand for completing the builder with an empty body, as is
    // the case with many HTTP request methods (e.g. GET).
    fn record(&mut self) -> Result<RequestRecord, FlError>;

    // Complete the builder with any request body that can be
    // converted to a single `hyper::Chunk`
    fn record_body<C>(&mut self, body: C) -> Result<RequestRecord, FlError>
        where C: Into<Chunk> + Clone;
}

impl RequestRecordable for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord, FlError> {
        let request = self.body(HyBody::empty())?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = BodyImage::empty();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body<C>(&mut self, body: C) -> Result<RequestRecord, FlError>
        where C: Into<Chunk> + Clone
    {
        let chunk_copy: Chunk = body.clone().into();
        let chunk: Chunk = body.into();
        let request = self.body(chunk.into())?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = if chunk_copy.is_empty() {
            BodyImage::empty()
        } else {
            let mut b = BodyImage::with_chunks_capacity(1);
            b.save(chunk_copy)?;
            b
        };

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request(url: &str) -> Result<RequestRecord, FlError> {
        http::Request::builder()
            .method(http::Method::GET)
            .header(http::header::ACCEPT,
                    "text/html, application/xhtml+xml, \
                     application/xml;q=0.9, \
                     */*;q=0.8" )
            .header(http::header::ACCEPT_LANGUAGE, "en")
            .header(http::header::ACCEPT_ENCODING, "gzip, deflate")
            .header(http::header::USER_AGENT,
                    "Mozilla/5.0 \
                     (compatible; hyper-bowl 0.1.0; \
                      +http://github.com/dekellum/hyper-bowl)")
            // Referer? Etag, If-Modified...?
            // "Connection: keep-alive" (header) is default for HTTP 1.1
            .uri(url)
            .record()
    }

    #[test]
    fn test_small_http() {
        let tune = Tunables::new();
        let req = create_request("http://gravitext.com").unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.res_body.is_ram());
        assert!(dl.res_body.len() > 0);
    }

    #[test]
    fn test_small_https() {
        let tune = Tunables::new();
        let req = create_request("https://example.com").unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.res_body.is_ram());
        assert!(dl.res_body.len() > 0);
    }

    #[test]
    fn test_not_found() {
        let tune = Tunables::new();
        let req = create_request("http://gravitext.com/no/existe").unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert_eq!(dl.status.as_u16(), 404);

        assert!(dl.res_body.is_ram());
        assert!(dl.res_body.len() > 0);
        assert!(dl.res_body.len() < 1000);
    }

    #[test]
    fn test_large_http() {
        let tune = Tuner::new()
            .set_max_body_ram(64 * 1024)
            .finish();
        let req = create_request(
            "http://gravitext.com/images/jakarta_slum.jpg"
        ).unwrap();
        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.res_body.len() > (64 * 1024));
        assert!(!dl.res_body.is_ram());
    }
}
