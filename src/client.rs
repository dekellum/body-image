//! HTTP client integration and utilities.
//!
//! This optional module (via non-default _client_ feature) provides
//! additional integration with the _futures_, _http_, _hyper_ 0.12.x., and
//! _tokio_ crates.
//!
//! * Trait [`RequestRecordable`](trait.RequestRecordable.html) extends
//!   `http::request::Builder` for recording a
//!   [`RequestRecord`](struct.RequestRecord.html), which can then be passed
//!   to `request_dialog` or `fetch`.
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
//!   fully asynchronous receipt of a `hyper::Body` stream.
//!
//! * The [`decode_res_body`](fn.decode_res_body.html) and associated
//!   functions will decompress any supported Transfer/Content-Encoding of the
//!   response body and update the `Dialog` accordingly.
//!
//! With the release of _hyper_ 0.12 and _tokio_ reform, the intent is to
//! evolve this module into a more general purpose _middleware_ type facility,
//! including:
//!
//! * More flexible integration of the recorded `Dialog` into more complete
//!   _tokio_ applications (partially complete).
//!
//! * Asynchronous I/O adaptions for file-based bodies where appropriate and
//!   beneficial (partially complete, see `AsyncBodySink`).
//!
//! * Symmetric support for `BodyImage`/`BodySink` request/response bodies.

extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate hyperx;
extern crate tokio;
extern crate tokio_threadpool;

#[cfg(test)] extern crate fern;
#[cfg(test)] extern crate hyper_stub;

use std::mem;
use std::time::Instant;

#[cfg(feature = "brotli")]
use brotli;

use bytes::Bytes;

/// Convenient and non-repetitive alias.
/// Also: "a sudden brief burst of bright flame or light."
use failure::Error as Flare;

use flate2::read::{DeflateDecoder, GzDecoder};
use self::futures::{future, Async, AsyncSink, Future, Poll, Sink, StartSend, Stream};
use http;
use self::hyper::{Chunk, Client};
use self::hyperx::header::{ContentEncoding, ContentLength,
                           Encoding as HyEncoding,
                           Header, TransferEncoding, Raw};
use self::tokio::timer::DeadlineError;
use self::tokio::util::FutureExt;

use {BodyImage, BodySink, BodyError, Encoding,
     Prolog, Dialog, RequestRecorded, Tunables, VERSION};

/// The HTTP request (with body) type
type HyRequest = http::Request<hyper::Body>;

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
/// function constructs a default *tokio* `Runtime`, `HttpsConnector`, and
/// *hyper* `Client` in a simplistic form internally, waiting with timeout,
/// and dropping these on completion.
pub fn fetch(rr: RequestRecord, tune: &Tunables) -> Result<Dialog, Flare> {
    let mut pool = tokio::executor::thread_pool::Builder::new();
    pool.name_prefix("tpool-")
        .pool_size(2)
        .max_blocking(2);
    let mut rt = tokio::runtime::Builder::new()
        .threadpool_builder(pool)
        .build().unwrap();
    let connector = hyper_tls::HttpsConnector::new(1 /*DNS threads*/)?;
    let client = Client::builder().build(connector);
    rt.block_on(request_dialog(&client, rr, tune))
    // Drop of `rt`, here, is equivalent to shutdown_now and wait
}

/// Given a suitable `Client` and `RequestRecord`, return a
/// `Future<Item=Dialog>`.  The provided `Tunables` governs timeout intervals
/// (initial response and complete body) and if the response `BodyImage` will
/// be in `Ram` or `FsRead`.
pub fn request_dialog<CN>(client: &Client<CN, hyper::Body>,
                          rr: RequestRecord,
                          tune: &Tunables)
    -> impl Future<Item=Dialog, Error=Flare> + Send
    where CN: hyper::client::connect::Connect + Sync + 'static
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
    -> Box<Future<Item=InDialog, Error=Flare> + Send>
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
        Err(e) => { return Box::new(future::err(e)); }
    };

    let async_body = AsyncBodySink::new(bsink, tune);

    let mut in_dialog = InDialog {
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        res_body:    BodySink::empty() // tmp, swap'ed below.
    };

    Box::new(
        body.from_err::<Flare>()
            .forward(async_body)
            .and_then(|(_strm, mut async_body)| {
                mem::swap(async_body.body_mut(), &mut in_dialog.res_body);
                Ok(in_dialog)
            })
    )
}

/// Adaptor for `BodySink` implementing the `futures::Sink` trait.  This
/// allows a `hyper::Body` stream to be forwarded (e.g. via
/// `futures::Stream::forward`) to a `BodySink`, in a fully asynchronous
/// fashion.
///
/// `Tunables` are used during the streaming to decide when to write back a
/// BodySink in `Ram` to `FsWrite`.  This implementation uses
/// `tokio_threadpool::blocking` to request becoming a backup thread for
/// blocking operations including `BodySink::write_back` and
/// `BodySink::write_all` (state `FsWrite`). It may thus only be used on the
/// tokio threadpool. If the `max_blocking` number of backup threads is
/// reached, and a blocking operation is required, then this implementation
/// will appear *full*, with `start_send` returning
/// `Ok(AsyncSink::NotReady(chunk)`, until a backup thread becomes available
/// or any timeout occurs.
pub struct AsyncBodySink {
    body: BodySink,
    tune: Tunables,
}

impl AsyncBodySink {

    /// Wrap `BodySink` and `Tunables` instance.
    pub fn new(body: BodySink, tune: Tunables) -> AsyncBodySink {
        AsyncBodySink { body, tune }
    }

    /// The inner `BodySink` as constructed.
    pub fn body(&self) -> &BodySink {
        &self.body
    }

    /// A mutable reference to the inner `BodySink`.
    pub fn body_mut(&mut self) -> &mut BodySink {
        &mut self.body
    }

    /// Unwrap and return the `BodySink`.
    pub fn into_inner(self) -> BodySink {
        self.body
    }
}

macro_rules! unblock {
    ($c:ident, || $b:block) => (match tokio_threadpool::blocking(|| $b) {
        Ok(Async::Ready(Ok(_))) => (),
        Ok(Async::Ready(Err(e))) => return Err(e.into()),
        Ok(Async::NotReady) => {
            debug!("No blocking backup thread available -> NotReady");
            return Ok(AsyncSink::NotReady($c));
        }
        Err(e) => return Err(e.into())
    })
}

impl Sink for AsyncBodySink {
    type SinkItem = Chunk;
    type SinkError = Flare;

    fn start_send(&mut self, chunk: Chunk) -> StartSend<Chunk, Flare> {
        let new_len = self.body.len() + (chunk.len() as u64);
        if new_len > self.tune.max_body() {
            bail!("Response stream too long: {}+", new_len);
        }
        if self.body.is_ram() && new_len > self.tune.max_body_ram() {
            unblock!(chunk, || {
                debug!("to write back file (blocking, len: {})", new_len);
                self.body.write_back(self.tune.temp_dir())
            })
        }
        if self.body.is_ram() {
            debug!("to save chunk (len: {})", chunk.len());
            self.body.save(chunk).map_err(Flare::from)?;
        } else {
            unblock!(chunk, || {
                debug!("to write chunk (blocking, len: {})", chunk.len());
                self.body.write_all(&chunk)
            })
        }

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), Flare> {
        Ok(Async::Ready(()))
    }

    fn close(&mut self) -> Poll<(), Flare> {
        Ok(Async::Ready(()))
    }
}

pub struct AsyncBodyImage {
    state: AsyncImageState,
    tune: Tunables,
}

impl AsyncBodyImage {
    pub fn new(body: BodyImage, tune: Tunables) -> AsyncBodyImage {
        AsyncBodyImage { state: AsyncImageState::Ram(body.into_vec().into_iter()),
                         tune }
    }
}

use std::vec::IntoIter;

enum AsyncImageState {
    Ram(IntoIter<Bytes>),
}

impl Stream for AsyncBodyImage
{
    type Item = Bytes;
    type Error = Flare;

    fn poll(&mut self) -> Poll<Option<Bytes>, Flare> {
        match self.state {
            AsyncImageState::Ram(ref mut iter) => {
                Ok(Async::Ready(iter.next()))
            }
        }
    }
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
/// _Limitations:_ This can't be `Clone`, because
/// `http::Request<client::hyper::Body>` isn't `Clone`.
#[derive(Debug)]
pub struct RequestRecord {
    request:      HyRequest,
    prolog:       Prolog,
}

impl RequestRecord {
    /// The HTTP method (verb), e.g. `GET`, `POST`, etc.
    pub fn method(&self)  -> &http::Method         { &self.prolog.method }

    /// The complete URL as used in the request.
    pub fn url(&self)     -> &http::Uri            { &self.prolog.url }

    /// Return the HTTP request.
    pub fn request(&self) -> &HyRequest            { &self.request }
}

impl RequestRecorded for RequestRecord {
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

/// Extension trait for `http::request::Builder`, to enable recording
/// key portions of the request for the final `Dialog`.
///
/// In particular any request body (e.g. POST, PUT) needs to be cloned in
/// advance of finishing the request, though this is inexpensive via
/// `Bytes::clone`.
///
/// _Limitation_: Currently only a single contiguous RAM buffer
/// (implementing `Into<Bytes>`) is supported as the request body.
pub trait RequestRecordable {
    /// Short-hand for completing the builder with an empty body, as is
    /// the case with many HTTP request methods (e.g. GET).
    fn record(&mut self) -> Result<RequestRecord, Flare>;

    /// Complete the builder with any request body that can be converted to a
    /// `Bytes` buffer.
    fn record_body<B>(&mut self, body: B) -> Result<RequestRecord, Flare>
        where B: Into<Bytes>;
}

impl RequestRecordable for http::request::Builder {
    fn record(&mut self) -> Result<RequestRecord, Flare> {
        let request = self.body(hyper::Body::empty())?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = BodyImage::empty();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body<B>(&mut self, body: B) -> Result<RequestRecord, Flare>
        where B: Into<Bytes>
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

#[cfg(test)]
mod tests {
    use ::std;
    use ::std::time::Duration;

    use ::log;

    use ::Tuner;
    use super::*;

    fn create_request(url: &str) -> Result<RequestRecord, Flare> {
        http::Request::builder()
            .method(http::Method::GET)
            .header(http::header::ACCEPT, BROWSE_ACCEPT)
            .header(http::header::ACCEPT_LANGUAGE, "en")
            .header(http::header::ACCEPT_ENCODING, ACCEPT_ENCODINGS)
            .header(http::header::USER_AGENT, &user_agent()[..])
            .uri(url)
            .record()
    }

    #[test]
    fn test_small_http() {
        assert!(*LOG_SETUP);
        let tune = Tunables::new();
        let req = create_request("http://gravitext.com").unwrap();

        let dl = fetch(req, &tune).unwrap();
        println!("Response {:#?}", dl);

        assert!(dl.res_body.is_ram());
        assert!(dl.res_body.len() > 0);
    }

    #[test]
    fn test_small_https() {
        assert!(*LOG_SETUP);
        let tune = Tunables::new();
        let req = create_request("https://www.usa.gov").unwrap();

        let dl = fetch(req, &tune).unwrap();
        let dl = dl.clone();
        println!("Response {:#?}", dl);

        assert!(dl.res_body.is_ram());
        assert!(dl.res_body.len() > 0);
    }

    #[test]
    fn test_not_found() {
        assert!(*LOG_SETUP);
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
        assert!(*LOG_SETUP);
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

    #[test]
    fn test_large_parallel_constrained() {
        assert!(*LOG_SETUP);
        let tune = Tuner::new()
            .set_max_body_ram(64 * 1024)
            .set_res_timeout(Duration::from_secs(15))
            .set_body_timeout(Duration::from_secs(55))
            .finish();

        let mut pool = tokio::executor::thread_pool::Builder::new();
        pool.name_prefix("tpool-")
            .pool_size(1)
            .max_blocking(2);
        let mut rt = tokio::runtime::Builder::new()
            .threadpool_builder(pool)
            .build().unwrap();

        let client = Client::new();

        let rq0 = create_request(
            "http://cache.ruby-lang.org/pub/ruby/1.8/ChangeLog-1.8.2"
        ).unwrap();
        let rq1 = create_request(
            "http://cache.ruby-lang.org/pub/ruby/1.8/ChangeLog-1.8.3"
        ).unwrap();

        let res = rt.block_on(
            request_dialog(&client, rq0, &tune)
                .join(request_dialog(&client, rq1, &tune))
        );
        match res {
            Ok((dl0, dl1)) => {
                assert_eq!(dl0.res_body.len(), 333_210);
                assert_eq!(dl1.res_body.len(), 134_827);
                assert!(!dl0.res_body.is_ram());
                assert!(!dl1.res_body.is_ram());
            }
            Err(e) => {
                panic!("failed with: {}", e);
            }
        }
    }

    #[test]
    fn test_dialog_via_stub() {
        assert!(*LOG_SETUP);
        let tune = Tunables::new();
        let rq = create_request(
            "http://gravitext.com/stubs/no/existe"
        ).unwrap();
        let client = hyper_stub::proxy_client_fn_ok(|_req| {
            hyper::Response::new("stub".into())
        });
        let mut rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(request_dialog(&client, rq, &tune));
        match res {
            Ok(dl) => {
                assert_eq!(dl.res_body.len(), 4);
            }
            Err(e) => {
                panic!("failed with: {}", e);
            }
        }
    }

    // Use lazy static to ensure we only setup logging once (by first test and
    // thread)
    lazy_static! {
        pub static ref LOG_SETUP: bool = setup_logger();
    }

    fn setup_logger() -> bool {
        let level = if let Ok(l) = std::env::var("TEST_LOG") {
            l.parse().unwrap()
        } else {
            0
        };
        if level == 0 { return true; }

        let mut disp = fern::Dispatch::new()
            .format(|out, message, record| {
                let t = std::thread::current();
                out.finish(format_args!(
                    "{} {} {}: {}",
                    record.level(),
                    record.target(),
                    t.name().map(str::to_owned)
                        .unwrap_or_else(|| format!("{:?}", t.id())),
                    message
                ))
            });
        disp = if level == 1 {
            disp.level(log::LevelFilter::Info)
        } else {
            disp.level(log::LevelFilter::Debug)
        };

        if level < 2 {
            // These are only for record/client deps, but are harmless if not
            // loaded.
            disp = disp
                .level_for("hyper::proto",  log::LevelFilter::Info)
                .level_for("tokio_core",    log::LevelFilter::Info)
                .level_for("tokio_reactor", log::LevelFilter::Info);
        }
        disp.chain(std::io::stderr())
            .apply().expect("setup logger");

        true
    }
}
