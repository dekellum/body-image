#[macro_use] extern crate failure;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate hyper_tls;
extern crate tempfile;
extern crate tokio_core;

// FIXME: Use atleast while prototyping. Might eventually switch to an
// error enum to get clear separation between hyper::Error and
// application errors.
use failure::Error as FlError;

use std::io::{stdout, Seek, SeekFrom, Write};
use std::fmt;
use std::fs::File;
use futures::{Future, Stream};
use futures::future::err as futerr;
use futures::future::result as futres;
use hyper::{Chunk, Client};
use hyper::client::compat::CompatFutureResponse;
use tokio_core::reactor::Core;
use tempfile::tempfile;

type HyRequest = http::Request<hyper::Body>;

/// Represents a resolved HTTP body payload via RAM or file-system
/// buffering strategies
enum BodyImage {
    Ram(Vec<Chunk>),
    Fs(File),
}

impl BodyImage {
    pub fn with_ram(size_estimate: u64) -> BodyImage {
        // Estimate chunks needed based on a plausible 8 KiB chunk size
        BodyImage::Ram(Vec::with_capacity((size_estimate / 0x2000 + 1) as usize))
    }

    pub fn with_fs() -> Result<BodyImage, FlError> {
        let f = tempfile()?;
        Ok(BodyImage::Fs(f))
    }

    /// Save chunk based on variant
    pub fn save(&mut self, chunk: Chunk) -> Result<(), FlError> {
        match *self {
            BodyImage::Ram(ref mut v) => {
                v.push(chunk);
                Ok(())
            }
            BodyImage::Fs(ref mut f) => {
                f.write_all(&chunk)
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

    /// Consumes self variant BodyImage::Ram, returning a BodyImage::Fs
    /// with all chunks written. Panics if self is not Ram.
    pub fn write_back(self) -> Result<BodyImage, FlError> {
        if let BodyImage::Ram(v) = self {
            let mut f = tempfile()?;
            for c in v {
                f.write_all(&c)?;
            }
            Ok(BodyImage::Fs(f))
        } else {
            panic!("Invalid state BodyImage(::Fs)::write_back");
        }
    }

    /// Prepare for consumption
    pub fn prepare(&mut self) -> Result<(), FlError> {
        if let BodyImage::Fs(ref mut f) = *self {
            f.seek(SeekFrom::Start(0))?;
        }
        Ok(())
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
            BodyImage::Fs(ref file) => {
                f.debug_tuple("Fs")
                    .field(file)
                    .finish()
            }
        }
    }
}

/// Response wrapper, preserving various fields from the Request
struct Prolog {
    method:       http::Method,
    uri:          http::Uri,
    req_headers:  http::HeaderMap,
    response:     http::Response<hyper::Body>,
}

/// An HTTP request and response recording.
#[derive(Debug)]
pub struct Dialog {
    method:       http::Method,
    uri:          http::Uri,
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
        self.body.prepare()?;
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
            max_body_ram:    102_400,
            max_body_len: 50_000_000,
        })
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

    fn write_headers(headers: &http::HeaderMap) -> Result<usize, FlError> {
        let mut out = stdout();
        let mut size = 0;
        for (key, value) in headers.iter() {
            size += out.write(key.as_ref())?;
            size += out.write(b": ")?;
            size += out.write(value.as_bytes())?;
            size += out.write(b"\r\n")?;
        }
        size += out.write(b"\r\n")?;
        Ok(size)
    }

    fn resp_future(&self, pl: Prolog)
        -> Box<Future<Item=Dialog, Error=FlError> + Send>
    {
        let (resp_parts, body) = pl.response.into_parts();

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

        let dl = Dialog {
            method:      pl.method,
            uri:         pl.uri,
            req_headers: pl.req_headers,
            version:     resp_parts.version,
            status:      resp_parts.status,
            res_headers: resp_parts.headers,
            body:        bf,
            body_len:    0u64,
        };

        let s = body
            .map_err(FlError::from)
            .fold(dl, move |mut dl, chunk| {
                let chunk_len = chunk.len() as u64;
                dl.body_len += chunk_len;
                if dl.body_len > max_body_len {
                    bail!("Response stream too long: {}+", dl.body_len);
                } else {
                    if dl.body.is_ram() && dl.body_len > max_body_ram {
                        dl.body = dl.body.write_back()?;
                    }
                    println!("to save chunk (len: {})", chunk_len);
                    dl.body
                        .save(chunk)
                        .and(Ok(dl))
                }
            });
        Box::new(s)
    }

    pub fn fetch(&self, req: HyRequest) -> Result<Dialog, FlError> {
        // FIXME: State of the Core (v Reactor), incl. construction,
        // use from multiple threads is under flux:
        // https://tokio.rs/blog/2018-02-tokio-reform -shipped/
        //
        // But hyper, as of 0.11.18 still depends on tokio-core, io,
        // service:
        // https://crates.io/crates/hyper
        let mut core = Core::new()?;
        let client = Client::configure()
            .connector(hyper_tls::HttpsConnector::new(4, &core.handle())?)
            // FIXME: threads ------------------------^
            .build(&core.handle());

        // FIXME: What about Timeouts? (Appears under flux)
        // https://github.com/hyperium/hyper/issues/1234
        // https://hyper.rs/guides/client/timeout/

        let method = req.method().clone();
        let uri = req.uri().clone();
        let req_headers = req.headers().clone();

        let fr: CompatFutureResponse = client.request_compat(req);

        let work = fr
            .map(|response| {
                Prolog { method, uri, req_headers, response }
            })
            .map_err(FlError::from)
            .and_then(|pl| self.resp_future(pl))
            .and_then(|dl| futres(dl.prepare()));

        // Run until completion
        core.run(work)
            .map_err(FlError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request(uri: &str) -> Result<HyRequest, FlError> {
        http::Request::builder()
            .method(http::Method::GET)
            .header(http::header::ACCEPT,
                    "text/html, application/xhtml+xml, application/xml; q=0.9, \
                     */*; q=0.8" )
            .header(http::header::ACCEPT_LANGUAGE, "en")
            .header(http::header::ACCEPT_ENCODING, "gzip, deflate")
            .header(http::header::USER_AGENT,
                    "Mozilla/5.0 \
                     (compatible; Iudex 1.4.0; +http://gravitext.com/iudex)")
            // "Connection: keep-alive" (header) is default for HTTP 1.1
            .uri(uri)
            .body(hyper::Body::empty())
            .map_err(FlError::from)
    }

    #[test]
    fn test_small_http() {
        let bw = HyperBowl::new().unwrap();
        let req = create_request("http://gravitext.com").unwrap();

        let dl = bw.fetch(req).unwrap();
        println!("Response {:#?}", dl);

        let bd = &dl.body;
        match bd {
            &BodyImage::Ram(_) => {},
            _ => panic!("Unexpected BodyForm {:?}", bd)
        }

        assert_eq!(dl.body_len, 8462);
    }

    #[test]
    fn test_not_found() {
        let bw = HyperBowl::new().unwrap();
        let req = create_request("http://gravitext.com/no/existe").unwrap();

        let dl = bw.fetch(req).unwrap();
        println!("Response {:#?}", dl);

        let bd = &dl.body;
        match bd {
            &BodyImage::Ram(_) => {},
            _ => panic!("Unexpected BodyForm {:?}", bd)
        }

        assert_eq!(dl.status.as_u16(), 404);
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

        let bd = &dl.body;
        match bd {
            &BodyImage::Fs(_) => {},
            _ => panic!("Unexpected BodyForm {:?}", bd)
        }
    }
}
