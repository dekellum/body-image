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
//!   asynchronous output as a `Stream` and `http_body::Body`.
//!
//! * Alternatively, [`UniBodySink`](struct.UniBodySink.html) and
//!   [`UniBodyImage`](struct.UniBodyImage.html) offer zero-copy `MemMap`
//!   support, using the custom [`UniBodyBuf`](struct.UniBodyBuf.html) item
//!   buffer type (instead of `Bytes`).
//!
//! * The [`decode_res_body`](fn.decode_res_body.html) and associated
//!   functions will decompress any supported Transfer/Content-Encoding of the
//!   response body and update the `Dialog` accordingly.

#![warn(rust_2018_idioms)]

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::time::Duration;

use bytes::Bytes;
use tao_log::warn;

use body_image::{
    BodyImage, BodySink, BodyError, Encoding,
    Prolog, RequestRecorded,
};

#[cfg(feature = "hyper_http")]
use body_image::{Epilog, Dialog};

/// Conveniently compact type alias for dyn Trait `std::error::Error`. It is
/// possible to query and downcast the type via methods of
/// [`std::any::Any`](https://doc.rust-lang.org/std/any/trait.Any.html).
pub type Flaw = Box<dyn StdError + Send + Sync + 'static>;

mod tune;
pub use tune::{FutioTunables, FutioTuner};

mod decode;
pub use decode::{decode_res_body, find_encodings, find_chunked};

mod image;
pub use image::{AsyncBodyImage, StreamWrapper};

mod sink;
pub use sink::{AsyncBodySink, SinkWrapper};

#[cfg(feature = "mmap")] mod mem_map_buf;
#[cfg(feature = "mmap")] use mem_map_buf::MemMapBuf;

mod uni_image;
pub use uni_image::{UniBodyImage, UniBodyBuf};

mod uni_sink;
pub use uni_sink::UniBodySink;

mod omni_image;
pub use omni_image::{
    Blocking, BlockingArbiter, LenientArbiter, StatefulArbiter,
    OmniBuf, OmniBodyImage
};

mod omni_sink;
pub use omni_sink::OmniBodySink;

#[cfg(feature = "hyper_http")] mod fetch;
#[cfg(feature = "hyper_http")] pub use self::fetch::{fetch, request_dialog};

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

    /// The `FutioTunables::res_timeout` duration was reached before receiving
    /// the initial response.
    ResponseTimeout(Duration),

    /// The `FutioTunables::body_timeout` duration was reached before receiving
    /// the complete response body.
    BodyTimeout(Duration),

    /// The content-length header exceeded `Tunables::max_body`.
    ContentLengthTooLong(u64),

    /// Error from _http_.
    Http(http::Error),

    /// Error from _hyper_.
    #[cfg(feature = "hyper_http")]
    Hyper(hyper::Error),

    /// Failed to decode an unsupported `Encoding`; such as `Compress`, or
    /// `Brotli`, when the _brotli_ feature is not enabled.
    UnsupportedEncoding(Encoding),

    /// Other unclassified errors.
    Other(Flaw),

    /// Unused variant to both enable non-exhaustive matching and warn against
    /// exhaustive matching.
    _FutureProof
}

impl fmt::Display for FutioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FutioError::Body(ref be) =>
                write!(f, "With body: {}", be),
            FutioError::ResponseTimeout(d) =>
                write!(f, "Timeout before initial response ({:?})", d),
            FutioError::BodyTimeout(d) =>
                write!(f, "Timeout before streaming body complete ({:?})", d),
            FutioError::ContentLengthTooLong(l) =>
                write!(f, "Response Content-Length too long: {}", l),
            FutioError::Http(ref e) =>
                write!(f, "Http error: {}", e),
            #[cfg(feature = "hyper_http")]
            FutioError::Hyper(ref e) =>
                write!(f, "Hyper error: {}", e),
            FutioError::UnsupportedEncoding(e) =>
                write!(f, "Unsupported encoding: {}", e),
            FutioError::Other(ref flaw) =>
                write!(f, "Other error: {}", flaw),
            FutioError::_FutureProof =>
                unreachable!("Don't abuse the _FutureProof!")
        }
    }
}

impl StdError for FutioError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match *self {
            FutioError::Body(ref be)         => Some(be),
            FutioError::Http(ref ht)         => Some(ht),
            #[cfg(feature = "hyper_http")]
            FutioError::Hyper(ref he)        => Some(he),
            FutioError::Other(ref flaw)      => Some(flaw.as_ref()),
            _ => None
        }
    }
}

impl From<BodyError> for FutioError {
    fn from(err: BodyError) -> FutioError {
        FutioError::Body(err)
    }
}

impl From<http::Error> for FutioError {
    fn from(err: http::Error) -> FutioError {
        FutioError::Http(err)
    }
}

#[cfg(feature = "hyper_http")]
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

// Note: There is intentionally no `From` implementation for `Flaw` to
// `FutioError::Other`, as that could easily lead to misclassification.
// Instead it should be manually constructed.

/// Return a generic HTTP user-agent header value for the crate, with version
pub fn user_agent() -> String {
    format!("Mozilla/5.0 (compatible; body-image {}; \
             +https://crates.io/crates/body-image)",
            VERSION)
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
#[cfg(feature = "hyper_http")]
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

#[cfg(feature = "hyper_http")]
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
    where B: http_body::Body + Send
{
    /// Short-hand for completing the builder with an empty body, as is
    /// the case with many HTTP request methods (e.g. GET).
    fn record(self) -> Result<RequestRecord<B>, http::Error>;

    /// Complete the builder with any body that can be converted to a (Ram)
    /// `Bytes` buffer.
    fn record_body<BB>(self, body: BB)
        -> Result<RequestRecord<B>, http::Error>
        where BB: Into<Bytes>;

    /// Complete the builder with a `BodyImage` for the request body.
    ///
    /// *Note*: Both `BodyImage` and `FutioTunables` are `Clone` (inexpensive),
    /// so that can be done beforehand to preserve owned copies.
    fn record_body_image(self, body: BodyImage, tune: FutioTunables)
        -> Result<RequestRecord<B>, http::Error>;
}

#[cfg(feature = "hyper_http")]
impl RequestRecorder<hyper::Body> for http::request::Builder {
    fn record(self) -> Result<RequestRecord<hyper::Body>, http::Error> {
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

    fn record_body<BB>(self, body: BB)
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

    fn record_body_image(self, body: BodyImage, tune: FutioTunables)
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
mod tests {
    mod forward;

    #[cfg(feature = "hyper_http")]
    mod server;

    /// These tests may fail because they depend on public web servers
    #[cfg(all(feature = "hyper_http", feature = "may_fail"))]
    mod live;

    use tao_log::{debug, debugv};
    use super::{FutioError, Flaw};
    use crate::logger::test_logger;
    use std::mem::size_of;

    fn is_flaw(f: Flaw) -> bool {
        debug!("Flaw Debug: {:?}, Display: \"{}\"", f, f);
        true
    }

    #[test]
    fn test_error_as_flaw() {
        assert!(test_logger());
        assert!(is_flaw(FutioError::ContentLengthTooLong(42).into()));
        assert!(is_flaw(FutioError::Other("one off".into()).into()));
    }

    #[test]
    fn test_error_size() {
        assert!(test_logger());
        assert!(debugv!(size_of::<FutioError>()) <= 32);
    }

    #[test]
    #[should_panic]
    fn test_error_future_proof() {
        assert!(!FutioError::_FutureProof.to_string().is_empty(),
                "should have panic'd before, unreachable")
    }
}
