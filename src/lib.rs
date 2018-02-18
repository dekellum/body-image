#[macro_use] extern crate failure;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate hyper_tls;
extern crate memmap;
extern crate tempfile;
extern crate tokio_core;

pub mod barc;

// FIXME: Use atleast while prototyping. Might switch to an error enum
// to get clear separation between hyper::Error and application
// errors.
use failure::Error as FlError;

use std::fmt;
use std::fs::File;
use std::io;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use futures::{Future, Stream};
use futures::future::err as futerr;
use futures::future::result as futres;
use hyper::{Chunk, Client};
use hyper::client::compat::CompatFutureResponse;
use memmap::Mmap;
use tempfile::tempfile;
use tokio_core::reactor::Core;

pub use hyper::Body as HyBody;

pub type HyRequest = http::Request<HyBody>;

/// Represents a resolved HTTP body payload via RAM or File-System
/// based buffering strategies
pub enum BodyImage {

    /// Body in random access memory, as a vector of separate chunks.
    Ram(Vec<Chunk>),

    /// Body in the process of being written to a (temporary) file
    FsWrite(File),

    /// Body in (temporary) file, ready for (one time, mutating)
    /// reading.
    FsRead(File),

    /// Body memory mapped (from file), for efficient, concurrent and
    /// repeated reading.
    MemMap(Mapped),
}

#[derive(Debug)]
pub struct Mapped {
    file: File,
    map: Mmap,
}

impl BodyImage {
    pub fn with_ram(size_estimate: u64) -> BodyImage {
        if size_estimate == 0 {
            BodyImage::Ram(Vec::with_capacity(0))
        } else {
            // Estimate capacity based on observed 8 KiB chunks
            BodyImage::Ram(
                Vec::with_capacity((size_estimate / 0x2000 + 1) as usize)
            )
        }
    }

    pub fn with_fs() -> Result<BodyImage, FlError> {
        let f = tempfile()?;
        Ok(BodyImage::FsWrite(f))
    }

    /// Save chunk based on variant
    pub fn save(&mut self, chunk: Chunk) -> Result<(), FlError> {
        match *self {
            BodyImage::Ram(ref mut v) => {
                v.push(chunk);
                Ok(())
            }
            BodyImage::FsWrite(ref mut f) => {
                f.write_all(&chunk)
            }
            _ => {
                panic!("Invalid state for save: {:?}", self);
            }

        }.map_err(FlError::from)
    }

    /// Return true if self variant is Ram
    pub fn is_ram(&self) -> bool {
        match *self {
            BodyImage::Ram(_) => true,
            _ => false
        }
    }

    /// Consumes self variant BodyImage::Ram, returning a
    /// BodyImage::FsWrite with all chunks written.
    /// Panics if self is not Ram.
    pub fn write_back(self) -> Result<BodyImage, FlError> {
        if let BodyImage::Ram(v) = self {
            let mut f = tempfile()?;
            for c in v {
                f.write_all(&c)?;
            }
            Ok(BodyImage::FsWrite(f))
        } else {
            panic!("Invalid state BodyImage(::!Ram)::write_back");
        }
    }

    /// Prepare for consumption
    pub fn prepare(self) -> Result<BodyImage, FlError> {
        if let BodyImage::FsWrite(mut f) = self {
            f.seek(SeekFrom::Start(0))?;
            Ok(BodyImage::FsRead(f))
        }
        else {
            Ok(self)
        }
    }

    /// Consumes self variant BodyImage::FsRead, returning a
    /// BodyImage::MemMap.  Panics if self is not Fs.
    fn map(self) -> Result<BodyImage, FlError> {
        if let BodyImage::FsRead(file) = self {
            let map = unsafe { Mmap::map(&file)? };
            Ok(BodyImage::MemMap(Mapped { file, map }))
        } else {
            panic!("Invalid state for map: {:?}", self);
        }
    }

    pub fn reader(&self) -> BodyReader {
        match *self {
            BodyImage::Ram(ref v) =>
                BodyReader::FromRam(ChunksReader::new(v)),
            BodyImage::FsWrite(_) =>
                panic!("Invalid state BodyImage::FsWrite::reader()"),
            BodyImage::FsRead(ref f) =>
                BodyReader::FromFs(f),
            BodyImage::MemMap(ref m) =>
                BodyReader::FromMemMap(Cursor::new(&m.map)),
        }
    }
}

pub enum BodyReader<'a> {
    FromRam(ChunksReader<'a>),
    FromFs(&'a File),
    FromMemMap(Cursor<&'a [u8]>),
}

impl<'a> BodyReader<'a> {
    pub fn as_read(&mut self) -> &mut Read {
        match *self {
            BodyReader::FromRam(ref mut cr) => cr,
            BodyReader::FromFs(ref mut f) => f,
            BodyReader::FromMemMap(ref mut cur) => cur,
        }
    }
}

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

impl fmt::Debug for BodyImage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BodyImage::Ram(ref v) => {
                // Avoids showing all chunks as u8 lists
                f.debug_struct("Ram(Vec<Chunk>)")
                    .field("capacity", &v.capacity())
                    .field("len", &v.len())
                    .finish()
            }
            BodyImage::FsRead(ref file) => {
                f.debug_tuple("FsRead")
                    .field(file)
                    .finish()
            }
            BodyImage::FsWrite(ref file) => {
                f.debug_tuple("FsWrite")
                    .field(file)
                    .finish()
            }
            BodyImage::MemMap(ref m) => {
                f.debug_tuple("MemMap")
                    .field(m)
                    .finish()
            }
        }
    }
}

/// Response wrapper, preserving various fields from the Request
struct Prolog {
    method:       http::Method,
    url:          http::Uri,
    req_headers:  http::HeaderMap,
    response:     http::Response<HyBody>,
}

/// An HTTP request and response recording.
#[derive(Debug)]
pub struct Dialog {
    method:       http::Method,
    url:          http::Uri,
    req_headers:  http::HeaderMap,
    version:      http::version::Version,
    status:       http::status::StatusCode,
    res_headers:  http::HeaderMap,
    body:         BodyImage,
    body_len:     u64,
}

impl Dialog {
    /// Prepare for consumption
    pub fn prepare(mut self) -> Result<Self, FlError> {
        self.body = self.body.prepare()?;
        Ok(self)
    }

    pub fn map_if_fs(mut self) -> Result<Self, FlError> {
        if let BodyImage::FsRead(_) = self.body {
            self.body = self.body.map()?;
        }
        Ok(self)
    }
}

/// Asynchronous recorder of HTTP request and response details as a
/// Dialog, with adaptive handling of bodies based on size.
pub struct HyperBowl {
    max_body_ram: u64,
    max_body_len: u64,
}

impl HyperBowl {
    pub fn new() -> Result<HyperBowl, FlError> {
        Ok(HyperBowl {
            max_body_ram:        96 * 1024,
            max_body_len: 48 * 1024 * 1024,
        })
    }

    pub fn fetch(&self, req: HyRequest) -> Result<Dialog, FlError> {
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

        let method = req.method().clone();
        let url = req.uri().clone();
        let req_headers = req.headers().clone();

        let fr: CompatFutureResponse = client.request_compat(req);

        let work = fr
            .map(|response| {
                Prolog { method, url, req_headers, response }
            })
            .map_err(FlError::from)
            .and_then(|prolog| self.resp_future(prolog))
            .and_then(|dialog| futres(dialog.prepare()));

        // FIXME: Handle content encoding (deflate, gzip) AFTER
        // completion (see libflate-rs)

        // Run until completion
        core.run(work)
            .map_err(FlError::from)
    }

    fn check_length(v: &http::header::HeaderValue, max: u64)
        -> Result<u64, FlError>
    {
        let v = v.to_str()?;
        let l: u64 = v.parse()?;
        if l > max {
            bail!("Response Content-Length too long: {}", l);
        }
        Ok(l)
    }

    fn resp_future(&self, prolog: Prolog)
        -> Box<Future<Item=Dialog, Error=FlError> + Send>
    {
        let (resp_parts, body) = prolog.response.into_parts();

        // Avoid borrowing self in below closures
        let max_body_ram = self.max_body_ram;
        let max_body_len = self.max_body_len;

        // Result<BodyImage> based on CONTENT_LENGTH header.
        let bf = match resp_parts.headers.get(http::header::CONTENT_LENGTH) {
            Some(v) => Self::check_length(v, max_body_len).and_then(|cl| {
                if cl > max_body_ram {
                    BodyImage::with_fs()
                } else {
                    Ok(BodyImage::with_ram(cl))
                }
            }),
            None => Ok(BodyImage::with_ram(max_body_ram))
        };

        // Unwrap BodyImage, returning any error as Future
        let bf = match bf {
            Ok(b) => b,
            Err(e) => { return Box::new(futerr(e)); }
        };

        let dialog = Dialog {
            method:      prolog.method,
            url:         prolog.url,
            req_headers: prolog.req_headers,
            version:     resp_parts.version,
            status:      resp_parts.status,
            res_headers: resp_parts.headers,
            body:        bf,
            body_len:    0u64,
        };

        let s = body
            .map_err(FlError::from)
            .fold(dialog, move |mut dialog, chunk| {
                let chunk_len = chunk.len() as u64;
                dialog.body_len += chunk_len;
                if dialog.body_len > max_body_len {
                    bail!("Response stream too long: {}+", dialog.body_len);
                } else {
                    if dialog.body.is_ram() && dialog.body_len > max_body_ram {
                        dialog.body = dialog.body.write_back()?;
                    }
                    println!("to save chunk (len: {})", chunk_len);
                    dialog.body
                        .save(chunk)
                        .and(Ok(dialog))
                }
            });
        Box::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request(url: &str) -> Result<HyRequest, FlError> {
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
                     (compatible; Iudex 1.4.0; +http://gravitext.com/iudex)")
            // Referer? Etag, If-Modified...?
            // "Connection: keep-alive" (header) is default for HTTP 1.1
            .uri(url)
            .body(HyBody::empty())
            .map_err(FlError::from)
    }

    #[test]
    fn test_small_http() {
        let bw = HyperBowl::new().unwrap();
        let req = create_request("http://gravitext.com").unwrap();

        let dl = bw.fetch(req).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.body.is_ram());
        assert_eq!(dl.body_len, 8462);
    }

    #[test]
    fn test_not_found() {
        let bw = HyperBowl::new().unwrap();
        let req = create_request("http://gravitext.com/no/existe").unwrap();

        let dl = bw.fetch(req).unwrap();
        println!("Response {:#?}", dl);

        assert_eq!(dl.status.as_u16(), 404);

        assert!(dl.body.is_ram());
        assert!(dl.body_len > 0);
        assert!(dl.body_len < 1000);
    }

    #[test]
    fn test_large_https() {
        let bw = HyperBowl::new().unwrap();
        let req = create_request(
            "https://sqoop.com/blog/2016-03-28-search-in-metropolitan-areas"
        ).unwrap();

        let dl = bw.fetch(req).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.body_len > 100_000 );
        assert!(!dl.body.is_ram());
    }
}
