#[macro_use] extern crate failure;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate tempfile;
extern crate tokio_core;

// FIXME: Use atleast while prototyping. Might eventually switch to an
// error enum to get clear seperation between hyper::Error and
// application errors.
use failure::Error as FlError;

use std::io::{stdout, Write};
use futures::{Future, Stream};
use futures::future::err as futerr;
use http::Request;
use hyper::Client;
use hyper::client::compat::CompatFutureResponse;
use tokio_core::reactor::Core;
use tempfile::tempfile;

// FIXME: Alt. names: Hyperbowl or hyperbole
pub struct BarcWriter {}

static MAX_BODY_LENGTH: u64 = 50_000;

// The Response with various aspects of the Request prepended.
struct ResponseComposite {
    method:   http::Method,
    uri:      http::Uri,
    req_headers: http::HeaderMap,
    response: http::Response<hyper::Body>,
}

impl BarcWriter {
    pub fn new() -> Result<BarcWriter, FlError> {
        Ok(BarcWriter {})
    }

    fn check_length(v: &http::header::HeaderValue) -> Result<u64, FlError> {
        let v = v.to_str()?;
        let l: u64 = v.parse()?;
        if l > MAX_BODY_LENGTH {
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

    fn resp_future(&mut self, rc: ResponseComposite)
        -> Box<Future<Item=u64, Error=FlError> + Send>
    {

        println!("meta: method: {}", rc.method);
        println!("meta: url: {}", rc.uri);
        println!("Request Headers:");
        if let Err(e) = BarcWriter::write_headers(&rc.req_headers) {
            return Box::new(futerr(e));
        }

        let (resp_parts, body) = rc.response.into_parts();

        println!("Response Status: {}", resp_parts.status);
        println!("Response Headers:");
        if let Err(e) = BarcWriter::write_headers(&resp_parts.headers) {
            return Box::new(futerr(e));
        }

        if let Some(v) = resp_parts.headers.get(http::header::CONTENT_LENGTH) {
            if let Err(e) = BarcWriter::check_length(v) {
                return Box::new(futerr(e));
            }
            // FIXME: Keep length for immediate decision to buffer to disk.
        }

        match tempfile() {
            Ok(mut tfile) => {
                let s = body.map_err(FlError::from).
                    fold(0u64, move |len_read, chunk| {
                        let chunk_len = chunk.len() as u64;
                        let new_len = len_read + chunk_len;
                        if new_len > MAX_BODY_LENGTH {
                            bail!("Response stream too long: {}+", new_len);
                        } else {
                            println!("to read chunk ({})", chunk_len);
                            tfile.write_all(&chunk).
                                map_err(FlError::from).
                                and(Ok(new_len))
                        }
                    });
                Box::new(s)
            }
            Err(e) => Box::new(futerr(e.into()))
        }
    }

    pub fn get(&mut self) -> Result<u64, FlError> {
        let mut core = Core::new()?;
        let client = Client::new(&core.handle());

        let uri = "http://gravitext.com";

        let req = Request::builder().
            method(http::Method::GET).
            uri(uri).
            body(hyper::Body::empty())?;

        let method = req.method().clone();
        let uri = req.uri().clone();
        let req_headers = req.headers().clone();

        let fr: CompatFutureResponse = client.request_compat(req);

        let work = fr.
            map(|response| {
                ResponseComposite { method, uri, req_headers, response } }).
            map_err(FlError::from).
            // -----(FnOnce(http::Response) -> IntoFuture<Error=FlError>)
            and_then(|res| self.resp_future(res));

        let len = core.run(work)?;
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get() {
        let mut bw = BarcWriter::new().unwrap();

        match bw.get() {
            Ok(len) => println!("Read: {} byte body", len),
            Err(e) => panic!("Error from work: {:?}", e)
        }
    }
}
