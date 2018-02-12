#[macro_use] extern crate failure;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate hyper_tls;
extern crate tempfile;
extern crate tokio_core;

// FIXME: Use atleast while prototyping. Might eventually switch to an
// error enum to get clear seperation between hyper::Error and
// application errors.
use failure::Error as FlError;

use std::io::{stdout, Seek, SeekFrom, Write};
use std::fs::File;
use futures::{Future, Stream};
use futures::future::err as futerr;
use futures::future::result as futres;
use http::Request;
use hyper::{Chunk, Client};
use hyper::client::compat::CompatFutureResponse;
use tokio_core::reactor::Core;
use tempfile::tempfile;

// FIXME: Just some (low) testing thresholds for now
static MAX_BODY_RAM: u64 =  5_000;
static MAX_BODY_LEN: u64 = 50_000;

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

struct ResponseInput {
    method:       http::Method,
    uri:          http::Uri,
    req_headers:  http::HeaderMap,
    max_body_len: u64,
    max_body_ram: u64,
    response:     http::Response<hyper::Body>,
}

struct ResponseOutput {
    method:       http::Method,
    uri:          http::Uri,
    req_headers:  http::HeaderMap,
    version:      http::version::Version,
    status:       http::status::StatusCode,
    res_headers:  http::HeaderMap,
    body:         BodyImage,
    body_len:     u64,
}

impl ResponseOutput {
    /// Prepare for consumption
    pub fn prepare(mut self) -> Result<Self, FlError> {
        self.body.prepare()?;
        Ok(self)
    }
}

/// Asynchronous recorder of HTTP request and response details, with
/// adaptive handling of bodies based on size.
pub struct HyperBowl {}

impl HyperBowl {
    pub fn new() -> Result<HyperBowl, FlError> {
        Ok(HyperBowl {})
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

    fn resp_future(&mut self, rc: ResponseInput)
        -> Box<Future<Item=ResponseOutput, Error=FlError> + Send>
    {
        let (resp_parts, body) = rc.response.into_parts();

        // Avoid borrow of rc in below closures
        let max_body_ram = rc.max_body_ram;
        let max_body_len = rc.max_body_len;

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

        let ro = ResponseOutput {
            method: rc.method,
            uri: rc.uri,
            req_headers: rc.req_headers,
            version: resp_parts.version,
            status: resp_parts.status,
            res_headers: resp_parts.headers,
            body: bf,
            body_len: 0u64,
        };

        let s = body
            .map_err(FlError::from)
            .fold(ro, move |mut ro, chunk| {
                let chunk_len = chunk.len() as u64;
                ro.body_len += chunk_len;
                if ro.body_len > max_body_len {
                    bail!("Response stream too long: {}+", ro.body_len);
                } else {
                    if ro.body.is_ram() && ro.body_len > max_body_ram {
                        ro.body = ro.body.write_back()?;
                    }
                    println!("to save chunk ({})", chunk_len);
                    ro.body
                        .save(chunk)
                        .and(Ok(ro))
                }
            });
        Box::new(s)
    }

    pub fn get(&mut self) -> Result<u64, FlError> {
        let mut core = Core::new()?;
        let client = Client::configure()
            .connector(hyper_tls::HttpsConnector::new(4, &core.handle())?)
            // FIXME: threads ------------------------^
            .build(&core.handle());

        let uri = "http://gravitext.com";

        let req = Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(hyper::Body::empty())?;

        let method = req.method().clone();
        let uri = req.uri().clone();
        let req_headers = req.headers().clone();
        let max_body_len = MAX_BODY_LEN;
        let max_body_ram = MAX_BODY_RAM;

        let fr: CompatFutureResponse = client.request_compat(req);

        let work = fr
            .map(|response| {
                ResponseInput { method, uri, req_headers,
                                max_body_len, max_body_ram,
                                response }
            })
            .map_err(FlError::from)
            .and_then(|ri| self.resp_future(ri))
            .and_then(|ro| futres(ro.prepare()));

        let ro = core.run(work)?;

        println!("meta: method: {}", ro.method);
        println!("meta: url: {}", ro.uri);
        println!("Request Headers:");
        Self::write_headers(&ro.req_headers)?;

        println!("Response Version: {:?}", ro.version);
        println!("Response Status: {}", ro.status);
        println!("Response Headers:");
        Self::write_headers(&ro.res_headers)?;

        Ok(ro.body_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get() {
        let mut bw = HyperBowl::new().unwrap();

        match bw.get() {
            Ok(len) => println!("Read: {} byte body", len),
            Err(e) => panic!("Error from work: {:?}", e)
        }
    }
}
