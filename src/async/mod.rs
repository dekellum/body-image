//! Asynchronous HTTP integration and utilities.
//!
//! This optional module (via default _async_ feature) provides
//! additional integration with the _futures_, _http_, _hyper_ 0.12.x., and
//! _tokio_ crates.
//!
//! * Traits [`RequestRecordableEmpty`](trait.RequestRecordableEmpty.html),
//!   [`RequestRecordableBytes`](trait.RequestRecordableBytes.html) and
//!   [`RequestRecordableImage`](trait.RequestRecordableImage.html) extend
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
//!   asynchronous input from a `hyper::Body` stream.
//!
//! * [`AsyncBodyImage`](struct.AsyncBodyImage.html) adapts a `BodyImage` for
//!   asynchronous output as a body stream.
//!
//! * The [`decode_res_body`](fn.decode_res_body.html) and associated
//!   functions will decompress any supported Transfer/Content-Encoding of the
//!   response body and update the `Dialog` accordingly.

extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate hyperx;
extern crate tokio;
extern crate tokio_threadpool;

mod body_image;
mod body_sink;

pub use self::body_sink::AsyncBodySink;
pub use self::body_image::AsyncBodyImage;

#[cfg(feature = "mmap")] mod memmap_body;
#[cfg(feature = "mmap")] pub use self::memmap_body::{AsyncMemMapBody, MemMapBuf};

use std::mem;
use std::time::Instant;

#[cfg(feature = "brotli")] use ::brotli;

use bytes::Bytes;

/// Convenient and non-repetitive alias.
/// Also: "a sudden brief burst of bright flame or light."
use failure::Error as Flare;

use flate2::read::{DeflateDecoder, GzDecoder};
use self::futures::{future, Future, Stream};
use self::futures::future::Either;

use http;
use self::hyperx::header::{ContentEncoding, ContentLength,
                           Encoding as HyEncoding,
                           Header, TransferEncoding, Raw};
use self::tokio::timer::DeadlineError;
use self::tokio::util::FutureExt;

use {BodyImage, BodySink, BodyError, Encoding,
     Prolog, Dialog, RequestRecorded, Tunables, VERSION};

/// Appropriate value for the HTTP accept-encoding request header, including
/// (br)otli when the brotli feature is configured.
#[cfg(feature = "brotli")]
pub static ACCEPT_ENCODINGS: &str          = "br, gzip, deflate";

/// Appropriate value for the HTTP accept-encoding request header, including
/// (br)otli when the brotli feature is configured.
#[cfg(not(feature = "brotli"))]
pub static ACCEPT_ENCODINGS: &str          = "gzip, deflate";

/// A browser-like HTTP accept request header value, with preference for
/// hypertext.
pub static BROWSE_ACCEPT: &str =
    "text/html, application/xhtml+xml, \
     application/xml;q=0.9, \
     */*;q=0.8";

/// Run an HTTP request to completion, returning the full `Dialog`. This
/// function constructs a default *tokio* `Runtime`,
/// `hyper_tls::HttpsConnector`, and `hyper::Client` in a simplistic form
/// internally, waiting with timeout, and dropping these on completion.
pub fn fetch<B>(rr: RequestRecord<B>, tune: &Tunables)
    -> Result<Dialog, Flare>
    where B: hyper::body::Payload + Send
{
    let mut pool = tokio::executor::thread_pool::Builder::new();
    pool.name_prefix("tpool-")
        .pool_size(2)
        .max_blocking(2);
    let mut rt = tokio::runtime::Builder::new()
        .threadpool_builder(pool)
        .build().unwrap();
    let connector = hyper_tls::HttpsConnector::new(1 /*DNS threads*/)?;
    let client = hyper::Client::builder().build(connector);
    rt.block_on(request_dialog(&client, rr, tune))
    // Drop of `rt`, here, is equivalent to shutdown_now and wait
}

/// Given a suitable `hyper::Client` and `RequestRecord`, return a
/// `Future<Item=Dialog>`.  The provided `Tunables` governs timeout intervals
/// (initial response and complete body) and if the response `BodyImage` will
/// be in `Ram` or `FsRead`.
pub fn request_dialog<CN, B>(client: &hyper::Client<CN, B>,
                             rr: RequestRecord<B>,
                             tune: &Tunables)
    -> impl Future<Item=Dialog, Error=Flare> + Send
    where CN: hyper::client::connect::Connect + Sync + 'static,
          B: hyper::body::Payload + Send
{
    let prolog = rr.prolog;
    let tune = tune.clone();

    let res_timeout = tune.res_timeout();
    let body_timeout = tune.body_timeout();
    let now = Instant::now();

    client.request(rr.request)
        .from_err::<Flare>()
        .map(|response| Monolog { prolog, response })
        .deadline(now + res_timeout)
        .map_err(move |de| {
            deadline_to_flare(de, || {
                format_err!("timeout before initial response ({:?})",
                            res_timeout)
            })
        })
        .and_then(|monolog| resp_future(monolog, tune))
        .deadline(now + body_timeout)
        .map_err(move |de| {
            deadline_to_flare(de, || {
                format_err!("timeout before streaming body complete ({:?})",
                            body_timeout)
            })
        })
        .and_then(InDialog::prepare)
}

fn deadline_to_flare<F>(de: DeadlineError<Flare>, on_elapsed: F) -> Flare
    where F: FnOnce() -> Flare
{
    if de.is_elapsed() {
        on_elapsed()
    } else if de.is_timer() {
        Flare::from(de.into_timer().unwrap())
    } else {
        de.into_inner().expect("inner")
    }
}

/// Return a list of supported encodings from the headers Transfer-Encoding
/// and Content-Encoding.  The `Chunked` encoding will be the first value if
/// found. At most one compression encoding will be the last value if found.
pub fn find_encodings(headers: &http::HeaderMap)-> Vec<Encoding> {
    let encodings = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .chain(headers
               .get_all(http::header::CONTENT_ENCODING)
               .iter());

    let mut chunked = false;
    let mut compress = None;

    'headers: for v in encodings {
        // Hyper's Content-Encoding includes Brotli (br) _and_
        // Chunked, is thus a super-set of Transfer-Encoding, so parse
        // all of these headers that way.
        if let Ok(v) = ContentEncoding::parse_header(&Raw::from(v.as_bytes())) {
            for av in v.iter() {
                match *av {
                    HyEncoding::Identity => {},
                    HyEncoding::Chunked => {
                        chunked = true
                    }
                    HyEncoding::Deflate => {
                        compress = Some(Encoding::Deflate);
                        break 'headers;
                    }
                    HyEncoding::Gzip => {
                        compress = Some(Encoding::Gzip);
                        break 'headers;
                    }
                    HyEncoding::Brotli => {
                        compress = Some(Encoding::Brotli);
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
    let mut encodings = Vec::with_capacity(2);
    if chunked {
        encodings.push(Encoding::Chunked);
    }
    if let Some(e) = compress {
        encodings.push(e);
    }
    encodings
}

/// Return true if the chunked Transfer-Encoding can be found in the headers.
pub fn find_chunked(headers: &http::HeaderMap) -> bool {
    let encodings = headers.get_all(http::header::TRANSFER_ENCODING);

    'headers: for v in encodings {
        if let Ok(v) = TransferEncoding::parse_header(&Raw::from(v.as_bytes()))
        {
            for av in v.iter() {
                match *av {
                    HyEncoding::Identity => {},
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
/// response body was decoded, `Ok(false)` if no or unsupported encoding,
/// or an error on failure.
pub fn decode_res_body(dialog: &mut Dialog, tune: &Tunables)
    -> Result<bool, BodyError>
{
    let encodings = find_encodings(&dialog.res_headers);

    let compression = encodings.last().and_then( |e| {
        if *e != Encoding::Chunked { Some(*e) } else { None }
    });

    let mut decoded = false;
    if let Some(comp) = compression {
        debug!("Body to {:?} decode: {:?}", comp, dialog.res_body);
        let new_body = decompress(&dialog.res_body, comp, tune)?;
        if let Some(b) = new_body {
            dialog.res_body = b;
            decoded = true;
            debug!("Body update: {:?}", dialog.res_body);
        } else {
            warn!("Unsupported encoding: {:?} not decoded", comp);
        }

    }

    dialog.res_decoded = encodings;

    Ok(decoded)
}

/// Decompress the provided body of any supported compression `Encoding`,
/// using `Tunables` for buffering and the final returned `BodyImage`. If the
/// encoding is not supported (e.g. `Chunked` or `Brotli`, without the feature
/// enabled), returns `None`.
pub fn decompress(body: &BodyImage, compression: Encoding, tune: &Tunables)
    -> Result<Option<BodyImage>, BodyError>
{
    let mut reader = body.reader();
    match compression {
        Encoding::Gzip => {
            let mut decoder = GzDecoder::new(reader.as_read());
            let len_est = body.len() * u64::from(tune.size_estimate_gzip());
            Ok(Some(BodyImage::read_from(&mut decoder, len_est, tune)?))
        }
        Encoding::Deflate => {
            let mut decoder = DeflateDecoder::new(reader.as_read());
            let len_est = body.len() * u64::from(tune.size_estimate_deflate());
            Ok(Some(BodyImage::read_from(&mut decoder, len_est, tune)?))
        }
        #[cfg(feature = "brotli")]
        Encoding::Brotli => {
            let mut decoder = brotli::Decompressor::new(
                reader.as_read(),
                tune.buffer_size_ram());
            let len_est = body.len() * u64::from(tune.size_estimate_brotli());
            Ok(Some(BodyImage::read_from(&mut decoder, len_est, tune)?))
        }
        _ => {
            Ok(None)
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
    -> impl Future<Item=InDialog, Error=Flare> + Send
{
    let (resp_parts, body) = monolog.response.into_parts();

    // Result<BodySink> based on CONTENT_LENGTH header.
    let bsink = match resp_parts.headers.get(http::header::CONTENT_LENGTH) {
        Some(v) => check_length(v, tune.max_body()).and_then(|cl| {
            if cl > tune.max_body_ram() {
                BodySink::with_fs(tune.temp_dir()).map_err(Flare::from)
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
        body.from_err::<Flare>()
            .forward(async_body)
            .and_then(|(_strm, mut async_body)| {
                mem::swap(async_body.body_mut(), &mut in_dialog.res_body);
                Ok(in_dialog)
            })
    )
}

fn check_length(v: &http::header::HeaderValue, max: u64)
    -> Result<u64, Flare>
{
    let l = *ContentLength::parse_header(&Raw::from(v.as_bytes()))?;
    if l > max {
        bail!("Response Content-Length too long: {}", l);
    }
    Ok(l)
}

/// An `http::Request` and recording. Note that other important getter
/// methods for `RequestRecord` are found in trait implementation
/// [`RequestRecorded`](#impl-RequestRecorded).
///
/// _Limitations:_ This can't be `Clone`, because `B = client::hyper::Body`
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
    fn prepare(self) -> Result<Dialog, Flare> {
        let res_decoded = if find_chunked(&self.res_headers) {
            vec![Encoding::Chunked]
        } else {
            Vec::with_capacity(0)
        };

        Ok(Dialog {
            prolog:      self.prolog,
            version:     self.version,
            status:      self.status,
            res_headers: self.res_headers,
            res_decoded,
            res_body:    self.res_body.prepare()?,
        })
    }
}

/// Type alias for body-image ≤0.3.0 compatibility
pub type RequestRecordable = RequestRecordableBytes<hyper::Body>;

/// Extension trait for `http::request::Builder`, to enable recording key
/// portions of the request for the final `Dialog`. This variant supports
/// recording empty bodies for typical request methods like `GET`.
///
/// Other request fields (`method`, `uri`, `headers`) are recorded by `clone`,
/// after finishing the request.
pub trait RequestRecordableEmpty<B>
    where B: hyper::body::Payload + Send
{
    /// Short-hand for completing the builder with an empty body, as is
    /// the case with many HTTP request methods (e.g. GET).
    fn record(&mut self) -> Result<RequestRecord<B>, Flare>;
}

/// Extension trait for `http::request::Builder`, to enable recording key
/// portions of the request for the final `Dialog`. This variant supports
/// recording bodies represented by an in-RAM `Bytes` buffer.
///
/// The request body is cloned in advance of finishing the request (internally
/// via `Builder::body`), though this is inexpensive via `Bytes::clone`. Other
/// request fields (`method`, `uri`, `headers`) are recorded by `clone`, after
/// finishing the request.
pub trait RequestRecordableBytes<B>: RequestRecordableEmpty<B>
    where B: hyper::body::Payload + Send
{
    /// Complete the builder with any body that can be converted to a (Ram)
    /// `Bytes` buffer.
    fn record_body<BB>(&mut self, body: BB)
        -> Result<RequestRecord<B>, Flare>
        where BB: Into<Bytes>;
}

/// Extension trait for `http::request::Builder`, to enable recording key
/// portions of the request for the final `Dialog`. This variant supports
/// recording full `BodyImage` request bodies.
///
/// The request body (e.g. POST, PUT) is cloned in advance of finishing the
/// request (internally via `Builder::body`), though this is inexpensive via
/// `BodyImage::clone`. Other request fields (`method`, `uri`, `headers`) are
/// recorded by `clone`, after finishing the request.
pub trait RequestRecordableImage<B>: RequestRecordableEmpty<B>
    where B: hyper::body::Payload + Send
{
    /// Complete the builder with a `BodyImage` for the request body.
    fn record_body_image(&mut self, body: BodyImage, tune: &Tunables)
        -> Result<RequestRecord<B>, Flare>;
}

impl RequestRecordableEmpty<hyper::Body> for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord<hyper::Body>, Flare> {
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
}

impl RequestRecordableBytes<hyper::Body> for http::request::Builder {
    fn record_body<BB>(&mut self, body: BB)
       -> Result<RequestRecord<hyper::Body>, Flare>
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
}

impl RequestRecordableImage<hyper::Body> for http::request::Builder {
    fn record_body_image(&mut self, body: BodyImage, tune: &Tunables)
        -> Result<RequestRecord<hyper::Body>, Flare>
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
mod tests {
    mod stub;
    mod server;

    #[cfg(feature = "live_test")]
    mod live;
}