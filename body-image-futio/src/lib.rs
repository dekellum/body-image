//! Asynchronous HTTP integration for _body-image_.
//!
//! The _body-image-futio_ crate integrates the _body-image_ crate with
//! _futures_, _http_, _hyper_ 0.12.x., and _tokio_ crates for both client and
//! server use.
//!
//! * Trait [`RequestRecorder`](trait.RequestRecorder.html) extends
//!   `http::request::Builder` for recording a
//!   [`RequestRecord`](struct.RequestRecord.html) of varous body types, which
//!   can then be passed to `request_dialog` or `fetch`.
//!
//! * The [`fetch`](fn.fetch.html) function runs a `RequestRecord` and returns
//!   a completed [`Dialog`](../struct.Dialog.html) using a single-use client
//!   and runtime for `request_dialog`.
//!
//! * The [`request_dialog`](fn.request_dialog.html) function returns a
//!   `Future<Item=Dialog>`, given a suitable `hyper::Client` reference and
//!   `RequestRecord`. This function is thus more composable for complete
//!   _tokio_ applications.
//!
//! * [`AsyncBodySink`](struct.AsyncBodySink.html) adapts a `BodySink` for
//!   asynchronous input from a (e.g. `hyper::Body`) `Stream`.
//!
//! * [`AsyncBodyImage`](struct.AsyncBodyImage.html) adapts a `BodyImage` for
//!   asynchronous output as a `Stream` and `hyper::body::Payload`.
//!
//! * Alternatively, [`UniBodySink`](struct.UniBodySink.html) and
//!   [`UniBodyImage`](struct.UniBodyImage.html) offer zero-copy `MemMap`
//!   support, using the custom [`UniBodyBuf`](struct.UniBodyBuf.html) item
//!   buffer type (instead of the `hyper::Chunk` or `Bytes`).
//!
//! * The [`decode_res_body`](fn.decode_res_body.html) and associated
//!   functions will decompress any supported Transfer/Content-Encoding of the
//!   response body and update the `Dialog` accordingly.

#![deny(dead_code, unused_imports)]
#![warn(rust_2018_idioms)]

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::mem;
use std::time::Duration;

#[cfg(feature = "brotli")] use brotli;

use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder};
use futures::{future, Future, Stream};
use futures::future::Either;
use http;
use hyper;
use hyper_tls;
use hyperx::header::{
    ContentEncoding, ContentLength, Encoding as HyEncoding,
    Header, TransferEncoding
};
use log::{debug, warn};
use tokio;
use tokio::timer::timeout;
use tokio::util::FutureExt;

use body_image::{
    BodyImage, BodySink, BodyError, Encoding,
    Epilog, Prolog, Dialog, Recorded, RequestRecorded, Tunables,
};

/// Conveniently compact type alias for dyn Trait `std::error::Error` errors,
/// generally used in test code, to represent many possible errors, or where
/// writing the concrete type would expose an implementation detail (e.g. of
/// an otherwise private dependency). It is possible to query and downcast the
/// type via methods of
/// [`std::any::Any`](https://doc.rust-lang.org/std/any/trait.Any.html).
pub type Flaw = Box<dyn StdError + Send + Sync + 'static>;

mod image;
pub use self::image::AsyncBodyImage;

mod sink;
pub use self::sink::AsyncBodySink;

#[cfg(feature = "mmap")] mod mem_map_buf;
#[cfg(feature = "mmap")] use self::mem_map_buf::MemMapBuf;

#[cfg(feature = "mmap")] mod uni_image;
#[cfg(feature = "mmap")] pub use self::uni_image::{UniBodyImage, UniBodyBuf};

#[cfg(feature = "mmap")] mod uni_sink;
#[cfg(feature = "mmap")] pub use self::uni_sink::UniBodySink;

/// The crate version string.
pub static VERSION: &str = env!("CARGO_PKG_VERSION");

/// Appropriate value for the HTTP accept-encoding request header, including
/// (br)otli when the brotli feature is configured.
#[cfg(feature = "brotli")]
pub static ACCEPT_ENCODINGS: &str = "br, gzip, deflate";

/// Appropriate value for the HTTP accept-encoding request header, including
/// (br)otli when the brotli feature is configured.
#[cfg(not(feature = "brotli"))]
pub static ACCEPT_ENCODINGS: &str = "gzip, deflate";

/// A browser-like HTTP accept request header value, with preference for
/// hypertext.
pub static BROWSE_ACCEPT: &str =
    "text/html, application/xhtml+xml, \
     application/xml;q=0.9, \
     */*;q=0.8";

/// Error enumeration for body-image-futio origin errors. This may be
/// extended in the future so exhaustive matching is gently discouraged with
/// an unused variant.
#[derive(Debug)]
pub enum FutioError {
    /// Error from `BodySink` or `BodyImage`.
    Body(BodyError),

    /// The `Tunables::res_timeout` duration was reached before receiving the
    /// initial response.
    ResponseTimeout(Duration),

    /// The `Tunables::body_timeout` duration was reached before receiving the
    /// complete response body.
    BodyTimeout(Duration),

    /// The content-length header exceeded `Tunables::max_body`.
    ContentLengthTooLong(u64),

    /// Error from _hyper_.
    Hyper(hyper::Error),

    /// Failed to decode an unsupported `Encoding`; such as `Compress`, or
    /// `Brotli`, when the _brotli_ feature is not enabled.
    UnsupportedEncoding(Encoding),

    /// Other unclassified errors.
    Other(Flaw),

    /// Unused variant to both enable non-exhaustive matching and warn against
    /// exhaustive matching.
    _FutureProof,
}

impl fmt::Display for FutioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FutioError::Body(ref be) =>
                write!(f, "With body: {}", be),
            FutioError::ResponseTimeout(d) =>
                write!(f,
                    "Timeout before initial response ({:?})", d),
            FutioError::BodyTimeout(d) =>
                write!(f,
                    "Timeout before streaming body complete ({:?})", d),
            FutioError::ContentLengthTooLong(l) =>
                write!(f, "Response Content-Length too long: {}", l),
            FutioError::Hyper(ref e) =>
                write!(f, "Hyper error: {}", e),
            FutioError::UnsupportedEncoding(e) =>
                write!(f, "Unsupported encoding: {}", e),
            FutioError::Other(ref flaw) =>
                write!(f, "Other error: {}", flaw),
            FutioError::_FutureProof => unreachable!()
        }
    }
}

impl StdError for FutioError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match *self {
            FutioError::Body(ref be) => Some(be),
            FutioError::Hyper(ref he) => Some(he),
            FutioError::Other(ref flaw) => Some(flaw.as_ref()),
            _ => None
        }
    }
}

impl From<BodyError> for FutioError {
    fn from(err: BodyError) -> FutioError {
        FutioError::Body(err)
    }
}

impl From<hyper::Error> for FutioError {
    fn from(err: hyper::Error) -> FutioError {
        FutioError::Hyper(err)
    }
}

impl From<io::Error> for FutioError {
    fn from(err: io::Error) -> FutioError {
        FutioError::Body(BodyError::Io(err))
    }
}

impl From<Flaw> for FutioError {
    fn from(flaw: Flaw) -> FutioError {
        FutioError::Other(flaw)
    }
}

/// Run an HTTP request to completion, returning the full `Dialog`. This
/// function constructs a default *tokio* `Runtime`,
/// `hyper_tls::HttpsConnector`, and `hyper::Client` in a simplistic form
/// internally, waiting with timeout, and dropping these on completion.
pub fn fetch<B>(rr: RequestRecord<B>, tune: &Tunables)
    -> Result<Dialog, FutioError>
    where B: hyper::body::Payload + Send
{
    let mut rt = tokio::runtime::Builder::new()
        .name_prefix("tpool-")
        .core_threads(2)
        .blocking_threads(2)
        .build()
        .unwrap();
    let connector = hyper_tls::HttpsConnector::new(1 /*DNS threads*/)
        .map_err(|e| FutioError::Other(Box::new(e)))?;
    let client = hyper::Client::builder().build(connector);
    rt.block_on(request_dialog(&client, rr, tune))
    // Drop of `rt`, here, is equivalent to shutdown_now and wait
}

/// Given a suitable `hyper::Client` and `RequestRecord`, return a
/// `Future<Item=Dialog>`.  The provided `Tunables` governs timeout intervals
/// (initial response and complete body) and if the response `BodyImage` will
/// be in `Ram` or `FsRead`.
pub fn request_dialog<CN, B>(
    client: &hyper::Client<CN, B>,
    rr: RequestRecord<B>,
    tune: &Tunables)
    -> impl Future<Item=Dialog, Error=FutioError> + Send
    where CN: hyper::client::connect::Connect + Sync + 'static,
          B: hyper::body::Payload + Send
{
    let prolog = rr.prolog;
    let tune = tune.clone();

    let res_timeout = tune.res_timeout();
    let body_timeout = tune.body_timeout();

    let futr = client
        .request(rr.request)
        .from_err::<FutioError>()
        .map(|response| Monolog { prolog, response });

    let futr = if let Some(t) = res_timeout {
        Either::A(futr
            .timeout(t)
            .map_err(move |te| {
                timeout_map(te, || FutioError::ResponseTimeout(t))
            })
        )
    } else {
        Either::B(futr)
    };

    let futr = futr.and_then(|monolog| resp_future(monolog, tune));

    let futr = if let Some(t) = body_timeout {
        Either::A(futr
            .timeout(t)
            .map_err(move |te| {
                timeout_map(te, || FutioError::BodyTimeout(t))
            })
        )
    } else {
        Either::B(futr)
    };

    futr.and_then(InDialog::prepare)
}

fn timeout_map<F>(te: timeout::Error<FutioError>, on_elapsed: F) -> FutioError
    where F: FnOnce() -> FutioError
{
    if te.is_elapsed() {
        on_elapsed()
    } else if te.is_timer() {
        FutioError::Other(te.into_timer().unwrap().into())
    } else {
        te.into_inner().expect("inner")
    }
}

/// Return a list of supported encodings from the headers Transfer-Encoding
/// and Content-Encoding.  The `Chunked` encoding will be the first value if
/// found. At most one compression encoding will be the last value if found.
pub fn find_encodings(headers: &http::HeaderMap) -> Vec<Encoding> {
    let encodings = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .chain(headers
               .get_all(http::header::CONTENT_ENCODING)
               .iter());

    let mut res = Vec::with_capacity(2);

    'headers: for v in encodings {
        // Hyper's Content-Encoding includes Brotli (br) _and_
        // Chunked, is thus a super-set of Transfer-Encoding, so parse
        // all of these headers that way.
        if let Ok(v) = ContentEncoding::parse_header(&v) {
            for av in v.iter() {
                match *av {
                    HyEncoding::Identity => {}
                    HyEncoding::Chunked => {
                        res.push(Encoding::Chunked);
                    }
                    HyEncoding::Deflate => {
                        res.push(Encoding::Deflate);
                        break 'headers;
                    }
                    HyEncoding::Gzip => {
                        res.push(Encoding::Gzip);
                        break 'headers;
                    }
                    HyEncoding::Brotli => {
                        res.push(Encoding::Brotli);
                        break 'headers;
                    }
                    _ => {
                        warn!("Found unknown encoding: {:?}", av);
                        break 'headers;
                    }
                }
            }
        }
    }
    res
}

/// Return true if the chunked Transfer-Encoding can be found in the headers.
pub fn find_chunked(headers: &http::HeaderMap) -> bool {
    let encodings = headers.get_all(http::header::TRANSFER_ENCODING);

    'headers: for v in encodings {
        if let Ok(v) = TransferEncoding::parse_header(&v) {
            for av in v.iter() {
                match *av {
                    HyEncoding::Identity => {}
                    HyEncoding::Chunked => {
                        return true;
                    }
                    _ => {
                        break 'headers;
                    }
                }
            }
        }
    }

    false
}

/// Decode the response body of the provided `Dialog` compressed with any
/// supported `Encoding`, updated the dialog accordingly.  The provided
/// `Tunables` controls decompression buffer sizes and if the final
/// `BodyImage` will be in `Ram` or `FsRead`. Returns `Ok(true)` if the
/// response body was decoded, or `Ok(false)` if no encoding was found, or an
/// error on failure, including from an unsupported `Encoding`.
pub fn decode_res_body(dialog: &mut Dialog, tune: &Tunables)
    -> Result<bool, FutioError>
{
    let encodings = find_encodings(dialog.res_headers());

    let compression = encodings.last().and_then(|e| {
        if *e != Encoding::Chunked { Some(*e) } else { None }
    });

    if let Some(comp) = compression {
        debug!("Body to {:?} decode: {:?}", comp, dialog.res_body());
        let new_body = decompress(dialog.res_body(), comp, tune)?;
        dialog.set_res_body_decoded(new_body, encodings);
        debug!("Body update: {:?}", dialog.res_body());
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Decompress the provided body of any supported compression `Encoding`,
/// using `Tunables` for buffering and the final returned `BodyImage`. If the
/// encoding is not supported (e.g. `Chunked` or `Brotli`, without the feature
/// enabled), returns `Err(FutioError::UnsupportedEncoding)`.
pub fn decompress(body: &BodyImage, compression: Encoding, tune: &Tunables)
    -> Result<BodyImage, FutioError>
{
    let reader = body.reader();
    match compression {
        Encoding::Gzip => {
            let mut decoder = GzDecoder::new(reader);
            let len_est = body.len() * u64::from(tune.size_estimate_gzip());
            Ok(BodyImage::read_from(&mut decoder, len_est, tune)?)
        }
        Encoding::Deflate => {
            let mut decoder = DeflateDecoder::new(reader);
            let len_est = body.len() * u64::from(tune.size_estimate_deflate());
            Ok(BodyImage::read_from(&mut decoder, len_est, tune)?)
        }
        #[cfg(feature = "brotli")]
        Encoding::Brotli => {
            let mut decoder = brotli::Decompressor::new(
                reader,
                tune.buffer_size_ram());
            let len_est = body.len() * u64::from(tune.size_estimate_brotli());
            Ok(BodyImage::read_from(&mut decoder, len_est, tune)?)
        }
        _ => {
            Err(FutioError::UnsupportedEncoding(compression))
        }
    }
}

/// Return a generic HTTP user-agent header value for the crate, with version
pub fn user_agent() -> String {
    format!("Mozilla/5.0 (compatible; body-image {}; \
             +https://crates.io/crates/body-image)",
            VERSION)
}

fn resp_future(monolog: Monolog, tune: Tunables)
    -> impl Future<Item=InDialog, Error=FutioError> + Send
{
    let (resp_parts, body) = monolog.response.into_parts();

    // Result<BodySink> based on CONTENT_LENGTH header.
    let bsink = match resp_parts.headers.get(http::header::CONTENT_LENGTH) {
        Some(v) => check_length(v, tune.max_body()).and_then(|cl| {
            if cl > tune.max_body_ram() {
                BodySink::with_fs(tune.temp_dir()).map_err(FutioError::from)
            } else {
                Ok(BodySink::with_ram(cl))
            }
        }),
        None => Ok(BodySink::with_ram(tune.max_body_ram()))
    };

    // Unwrap BodySink, returning any error as Future
    let bsink = match bsink {
        Ok(b) => b,
        Err(e) => { return Either::A(future::err(e)); }
    };

    let async_body = AsyncBodySink::new(bsink, tune);

    let mut in_dialog = InDialog {
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        res_body:    BodySink::empty() // tmp, swap'ed below.
    };

    Either::B(
        body.from_err::<FutioError>()
            .forward(async_body)
            .and_then(|(_strm, mut async_body)| {
                mem::swap(async_body.body_mut(), &mut in_dialog.res_body);
                Ok(in_dialog)
            })
    )
}

fn check_length(v: &http::header::HeaderValue, max: u64)
    -> Result<u64, FutioError>
{
    let l = *ContentLength::parse_header(&v)
        .map_err(|e| FutioError::Other(Box::new(e)))?;
    if l > max {
        return Err(FutioError::ContentLengthTooLong(l));
    }
    Ok(l)
}

/// An `http::Request` and recording. Note that other important getter
/// methods for `RequestRecord` are found in trait implementation
/// [`RequestRecorded`](#impl-RequestRecorded).
///
/// _Limitations:_ This can't be `Clone`, because `http::Request` currently
/// isn't `Clone`.  Also note that as used as type `B`, `hyper::Body` also
/// isn't `Clone`.
#[derive(Debug)]
pub struct RequestRecord<B> {
    request:      http::Request<B>,
    prolog:       Prolog,
}

impl<B> RequestRecord<B> {
    /// The HTTP method (verb), e.g. `GET`, `POST`, etc.
    pub fn method(&self)  -> &http::Method         { &self.prolog.method }

    /// The complete URL as used in the request.
    pub fn url(&self)     -> &http::Uri            { &self.prolog.url }

    /// Return the HTTP request.
    pub fn request(&self) -> &http::Request<B>     { &self.request }
}

impl<B> RequestRecorded for RequestRecord<B> {
    fn req_headers(&self) -> &http::HeaderMap      { &self.prolog.req_headers }
    fn req_body(&self)    -> &BodyImage            { &self.prolog.req_body }
}

/// Temporary `http::Response` wrapper, with preserved request
/// recording.
#[derive(Debug)]
struct Monolog {
    prolog:       Prolog,
    response:     http::Response<hyper::Body>,
}

/// An HTTP request with response in progress of being received.
#[derive(Debug)]
struct InDialog {
    prolog:       Prolog,
    version:      http::Version,
    status:       http::StatusCode,
    res_headers:  http::HeaderMap,
    res_body:     BodySink,
}

impl InDialog {
    // Convert to `Dialog` by preparing the response body and adding an
    // initial res_decoded for Chunked, if hyper handled chunked transfer
    // encoding.
    fn prepare(self) -> Result<Dialog, FutioError> {
        let res_decoded = if find_chunked(&self.res_headers) {
            vec![Encoding::Chunked]
        } else {
            Vec::with_capacity(0)
        };

        Ok(Dialog::new(
            self.prolog,
            Epilog {
                version:     self.version,
                status:      self.status,
                res_headers: self.res_headers,
                res_body:    self.res_body.prepare()?,
                res_decoded,
            }
        ))
    }
}

/// Extension trait for `http::request::Builder`, to enable recording key
/// portions of the request for the final `Dialog`.
///
/// Other request fields (`method`, `uri`, `headers`) are recorded by `clone`,
/// after finishing the request.

/// The request body is cloned in advance of finishing the request, though
/// this is inexpensive via `Bytes::clone` or `BodyImage::clone`. Other
/// request fields (`method`, `uri`, `headers`) are recorded by `clone`, after
/// finishing the request.
pub trait RequestRecorder<B>
    where B: hyper::body::Payload + Send
{
    /// Short-hand for completing the builder with an empty body, as is
    /// the case with many HTTP request methods (e.g. GET).
    fn record(&mut self) -> Result<RequestRecord<B>, http::Error>;

    /// Complete the builder with any body that can be converted to a (Ram)
    /// `Bytes` buffer.
    fn record_body<BB>(&mut self, body: BB)
        -> Result<RequestRecord<B>, http::Error>
        where BB: Into<Bytes>;

    /// Complete the builder with a `BodyImage` for the request body.
    fn record_body_image(&mut self, body: BodyImage, tune: &Tunables)
        -> Result<RequestRecord<B>, http::Error>;
}

impl RequestRecorder<hyper::Body> for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord<hyper::Body>, http::Error> {
        let request = self.body(hyper::Body::empty())?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = BodyImage::empty();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body }
        })
    }

    fn record_body<BB>(&mut self, body: BB)
        -> Result<RequestRecord<hyper::Body>, http::Error>
        where BB: Into<Bytes>
    {
        let buf: Bytes = body.into();
        let buf_copy: Bytes = buf.clone();
        let request = self.body(buf.into())?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = if buf_copy.is_empty() {
            BodyImage::empty()
        } else {
            BodyImage::from_slice(buf_copy)
        };

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body_image(&mut self, body: BodyImage, tune: &Tunables)
        -> Result<RequestRecord<hyper::Body>, http::Error>
    {
        let request = if !body.is_empty() {
            let stream = AsyncBodyImage::new(body.clone(), tune);
            self.body(hyper::Body::wrap_stream(stream))?
        } else {
            self.body(hyper::Body::empty())?
        };
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body: body } })
    }
}

#[cfg(test)]
mod logger;

#[cfg(test)]
mod futio_tests {
    #[cfg(feature = "mmap")]        mod futures;
                                    mod server;

    /// These tests may fail because they depend on public web servers
    #[cfg(feature = "may_fail")]    mod live;
}
