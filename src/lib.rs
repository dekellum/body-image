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

/// Alias for `hyper::Body`
pub use hyper::Body as HyBody;

pub type HyRequest = http::Request<HyBody>;

/// Represents an HTTP body payload via RAM or File-System
/// based buffering strategies.
#[derive(Debug)]
pub struct BodyImage {
    inner: BodyImageInner,
    len: u64
}

enum BodyImageInner {
    /// Body in random access memory, as a vector of separate chunks.
    Ram(Vec<Chunk>),

    /// Body in the process of being written to a temporary file
    FsWrite(File),

    /// Body in temporary file, ready for mutating, sequential read.
    FsRead(File),

    /// Body memory mapped (from file), for efficient, concurrent and
    /// repeated reading.
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
    pub fn empty() -> BodyImage {
        BodyImage::with_chunks_capacity(0)
    }

    pub fn with_ram(size_estimate: u64) -> BodyImage {
        if size_estimate == 0 {
            BodyImage::empty()
        } else {
            // Estimate capacity based on observed 8 KiB chunks
            let chunks = (size_estimate / 0x2000 + 1) as usize;
            BodyImage::with_chunks_capacity(chunks)
        }
    }

    pub fn with_chunks_capacity(cap: usize) -> BodyImage {
        BodyImage {
            inner: BodyImageInner::Ram(Vec::with_capacity(cap)),
            len: 0
        }
    }

    pub fn with_fs() -> Result<BodyImage, FlError> {
        let f = tempfile()?;
        Ok(BodyImage {
            inner: BodyImageInner::FsWrite(f),
            len: 0
        })
    }

    fn with_map(mapped: Mapped) -> BodyImage {
        let len = mapped.map.len() as u64;
        BodyImage {
            inner: BodyImageInner::MemMap(mapped),
            len
        }
    }

    /// Return true if self variant is `BodyImageInner::Ram`
    pub fn is_ram(&self) -> bool {
        match self.inner {
            BodyImageInner::Ram(_) => true,
            _ => false
        }
    }

    /// Return true if self represents an empty body.
    pub fn is_empty(&self) -> bool {
        match self.inner {
            BodyImageInner::Ram(ref v) => v.is_empty(),
            _ => false
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Save `Chunk` based on variant, appending to `BodyImageInner::Ram`
    /// or writing to `BodyImageInner::FsWrite`. Panics if in some other
    /// state.
    pub fn save(&mut self, chunk: Chunk) -> Result<(), FlError> {
        let len = chunk.len() as u64;
        match self.inner {
            BodyImageInner::Ram(ref mut v) => {
                v.push(chunk);
            }
            BodyImageInner::FsWrite(ref mut f) => {
                f.write_all(&chunk)?;
            }
            _ => {
                panic!("Invalid state for save(): {:?}", self);
            }
        }
        self.len += len;
        Ok(())
    }

    /// Consumes self variant `BodyImageInner::Ram` and returns a
    /// `BodyImageInner::FsWrite` with all chunks written.
    /// Panics if in some other state.
    pub fn write_back(&mut self) -> Result<(), FlError> {
        self.inner = if let BodyImageInner::Ram(ref v) = self.inner {
            let mut f = tempfile()?;
            for c in v {
                f.write_all(c)?;
            }
            BodyImageInner::FsWrite(f)
        } else {
            panic!("Invalid state for write_back(): {:?}", self);
        };
        Ok(())
    }

    /// Write all of slice to `BodyImageInner::FsWrite`. This can be an
    /// optimization over `BodyImageInner::save` when the state is
    /// known. Panics if in some other state.
    pub fn write_all(&mut self, buf: &[u8]) -> Result<(), FlError> {
        if let BodyImageInner::FsWrite(ref mut f) = self.inner {
            f.write_all(buf)?;
            self.len += buf.len() as u64;
            Ok(())
        }
        else {
            panic!("Invalid state for write_all(): {:?}", self);
        }
    }

    /// Prepare for (re-)reading. Converts `BodyImageInner::FsWrite` to
    /// `BodyImageInner::FsRead`.  Seeks to beginning for either of these
    /// states. No-op for other states.
    pub fn prepare(mut self) -> Result<Self, FlError> {
        match self.inner {
            BodyImageInner::FsWrite(mut f) => {
                f.flush()?;
                f.seek(SeekFrom::Start(0))?;
                self.inner = BodyImageInner::FsRead(f);
            }
            BodyImageInner::FsRead(ref mut f) => {
                f.seek(SeekFrom::Start(0))?;
            }
            _ => {}
        }
        Ok(self)
    }

    /// Consumes self variant `BodyImageInner::FsRead`, returning a
    /// `BodyImageInner::MemMap` by memory mapping the file.  Panics if
    /// self is in some other state.
    pub fn map(mut self) -> Result<Self, FlError> {
        if let BodyImageInner::FsRead(file) = self.inner {
            // FIXME: Check zero length case?
            let map = unsafe { Mmap::map(&file)? };
            self.inner = BodyImageInner::MemMap(Mapped { map, _file: file });
            Ok(self)
        } else {
            panic!("Invalid state for map(): {:?}", self);
        }
    }

    /// Return a new BodyReader over self. Panics if self is
    /// `BodyImageInner::FsWrite`. Use `BodyImageInner::prepare` first.
    pub fn reader(&self) -> BodyReader {
        match self.inner {
            BodyImageInner::Ram(ref v) =>
                BodyReader::FromRam(ChunksReader::new(v)),
            BodyImageInner::FsWrite(_) =>
                panic!("Invalid state BodyImageInner::FsWrite::reader()"),
            BodyImageInner::FsRead(ref f) =>
                BodyReader::FromFs(f),
            BodyImageInner::MemMap(ref m) =>
                BodyReader::FromMemMap(Cursor::new(&m.map)),
        }
    }

    /// Specialized and efficient write for states `BodyImageInner::Ram`
    /// and `BodyImageInner::MemMap` that can be written without mutating
    /// self. Panics if not one of those states.
    pub fn write_to(&self, out: &mut Write) -> Result<u64, FlError> {
        match self.inner {
            BodyImageInner::Ram(ref v) => {
                let mut size: u64 = 0;
                for c in v {
                    let b = &c;
                    out.write_all(b)?;
                    size += b.len() as u64;
                }
                Ok(size)
            }
            BodyImageInner::MemMap(ref m) => {
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

impl fmt::Debug for BodyImageInner {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BodyImageInner::Ram(ref v) => {
                // Avoids showing all chunks as u8 lists
                f.debug_struct("Ram(Vec<Chunk>)")
                    .field("capacity", &v.capacity())
                    .field("len", &v.len())
                    .finish()
            }
            BodyImageInner::FsRead(ref file) => {
                f.debug_tuple("FsRead")
                    .field(file)
                    .finish()
            }
            BodyImageInner::FsWrite(ref file) => {
                f.debug_tuple("FsWrite")
                    .field(file)
                    .finish()
            }
            BodyImageInner::MemMap(ref m) => {
                f.debug_tuple("MemMap")
                    .field(m)
                    .finish()
            }
        }
    }
}

/// Reader for `BodyImage` variants.
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

/// Specialized Reader for `BodyImageInner::Ram`
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
pub struct Prolog {
    method:       http::Method,
    url:          http::Uri,
    req_headers:  http::HeaderMap,
    req_body:     BodyImage,
}

/// An `http::Request` with extracted Prolog.
pub struct RequestRecord {
    request:      HyRequest,
    prolog:       Prolog,
}

/// Temporary `http::Response` wrapper, preserving Prolog.
struct Monolog {
    prolog:       Prolog,
    response:     http::Response<HyBody>,
}

/// An HTTP request and response recording.
#[derive(Debug)]
pub struct Dialog {
    meta:         http::HeaderMap,
    prolog:       Prolog,
    version:      http::version::Version,
    status:       http::status::StatusCode,
    res_headers:  http::HeaderMap,
    body:         BodyImage,
}

static META_URL: &'static [u8]             = b"url";
static META_METHOD: &'static [u8]          = b"method";
static META_RES_VERSION: &'static [u8]     = b"response-version";
static META_RES_STATUS: &'static [u8]      = b"response-status";

/// List of encodings decoded, in HTTP content-encoding headers format
static META_RES_DECODED: &'static [u8]     = b"response-decoded";

impl Dialog {
    /// Prepare for consumption
    pub fn prepare(mut self) -> Result<Self, FlError> {
        self.body = self.body.prepare()?;
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

        // FIXME: Rely on debug format of version for now. Should probably
        // replace this with match and custom representation.
        let v = format!("{:?}", self.version);
        hs.append(HeaderName::from_lowercase(META_RES_VERSION).unwrap(),
                  v.parse()?);

        hs.append(HeaderName::from_lowercase(META_RES_STATUS).unwrap(),
                  self.status.to_string().parse()?);
        Ok(hs)
    }

    /// If body is `BodyImageInner::FsRead`, convert to
    /// `BodyImageInner::MemMap` via `BodyImageInner::map`, else
    /// no-op.
    pub fn map_if_fs(mut self) -> Result<Self, FlError> {
        if let BodyImageInner::FsRead(_) = self.body.inner {
            self.body = self.body.map()?;
        }
        Ok(self)
    }
}

/// A collection of size limits and performance tuning constants.
#[derive(Clone, Copy)]
pub struct Tunables {
    max_body_ram:            u64,
    max_body:                u64,
    decode_buffer_ram:       usize,
    decode_buffer_fs:        usize,
    deflate_size_x_est:      u16,
    gzip_size_x_est:         u16,
}

impl Tunables {
    pub fn new() -> Result<Tunables, FlError> {
        Ok(Tunables {
            max_body_ram:        96 * 1024,
            max_body:     48 * 1024 * 1024,
            decode_buffer_ram:    8 * 1024,
            decode_buffer_fs:    64 * 1024,
            deflate_size_x_est:          4,
            gzip_size_x_est:             5,
        })
    }

    // FIXME: Add builder interface, setters
}

// Run an HTTP request to completion, returning the full `Dialog`
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
    let bf = match resp_parts.headers.get(http::header::CONTENT_LENGTH) {
        Some(v) => check_length(v, tune.max_body).and_then(|cl| {
            if cl > tune.max_body_ram {
                BodyImage::with_fs()
            } else {
                Ok(BodyImage::with_ram(cl))
            }
        }),
        None => Ok(BodyImage::with_ram(tune.max_body_ram))
    };

    // Unwrap BodyImage, returning any error as Future
    let bf = match bf {
        Ok(b) => b,
        Err(e) => { return Box::new(futerr(e)); }
    };

    let dialog = Dialog {
        meta:        http::HeaderMap::with_capacity(0),
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        body:        bf,
    };

    let s = body
        .map_err(FlError::from)
        .fold(dialog, move |mut dialog, chunk| {
            let new_len = dialog.body.len() + (chunk.len() as u64);
            if new_len > tune.max_body {
                bail!("Response stream too long: {}+", new_len);
            } else {
                if dialog.body.is_ram() && new_len > tune.max_body_ram {
                    dialog.body.write_back()?;
                }
                println!("to save chunk (len: {})", chunk.len());
                dialog.body
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
/// key portions of the request for the final `Dialog`. In particular
/// any request body (e.g. POST, PUT) needs to be cloned in advance of
/// finishing the request, because `hyper::Body` isn't `Clone`.
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
        let tune = Tunables::new().unwrap();
        let req = create_request("http://gravitext.com").unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.body.is_ram());
        assert_eq!(dl.body.len(), 8462);
    }

    #[test]
    fn test_not_found() {
        let tune = Tunables::new().unwrap();
        let req = create_request("http://gravitext.com/no/existe").unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert_eq!(dl.status.as_u16(), 404);

        assert!(dl.body.is_ram());
        assert!(dl.body.len() > 0);
        assert!(dl.body.len() < 1000);
    }

    #[test]
    fn test_large_https() {
        let tune = Tunables::new().unwrap();
        let req = create_request(
            "https://sqoop.com/blog/2016-03-28-search-in-metropolitan-areas"
        ).unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.body.len() > 100_000 );
        assert!(!dl.body.is_ram());
    }
}
