//! HTTP client integration and utilities.
//!
//! This optional module (via non-default _client_ feature) provides
//! additional integration with the _http_ crate and _hyper_ 0.11.x (with the
//! _compat_ feature, and its other dependencies.).  Thus far, its primary
//! motivation has been to support the `barc record` command line, though some
//! methods may have more general utility:
//!
//! * Trait [`RequestRecordable`](trait.RequestRecordable.html) extends
//!   `http::request::Builder` for recording a
//!   [`RequestRecord`](struct.RequestRecord.html), which can then be passed
//!   to `fetch`.
//!
//! * The [`fetch` function](fn.fetch.html) runs a `RequestRecord` and returns a
//!   completed [`Dialog`](../struct.Dialog.html).
//!
//! * The [`decode_res_body` function](fn.decode_res_body.html) and some
//!   related functions will decompress any supported Transfer/Content-Encoding
//!   of the response body and update the `Dialog` accordingly.
//!
//! Starting with the significant expected changes for _hyper_ 0.12 and its
//! dependencies, the intent is to evolve this module into a more general
//! purpose _middleware_ type facility, including:
//!
//! * More flexible integration of the recorded `Dialog` into more complete
//!   _hyper_ applications or downstream crate and frameworks.
//!
//! * Symmetric support for `BodySink`/`BodyImage` request bodies.
//!
//! * Asynchronous I/O adaptions for file-based bodies where appropriate and
//!   beneficial.

extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate tokio_core;

#[cfg(feature = "brotli")]
use brotli;

use bytes::Bytes;

/// Convenient and non-repetitive alias
/// Also: "a sudden brief burst of bright flame or light."
use failure::Error as Flare;

use flate2::read::{DeflateDecoder, GzDecoder};
use self::futures::{future, Future, Stream};
use http;
use self::hyper::Client;
use self::hyper::client::compat::CompatFutureResponse;
use self::hyper::header::{ContentEncoding, ContentLength,
                          Encoding as HyEncoding,
                          Header, TransferEncoding, Raw};
use self::tokio_core::reactor::Core;

use {BodyImage, BodySink, BodyError, Encoding,
     Prolog, Dialog, RequestRecorded, Tunables, VERSION};

/// The HTTP request (with body) type (as of hyper 0.11.x.)
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
/// function constructs all the necesarry _hyper_ and _tokio_ components in a
/// simplistic form internally, and is currently not recommended for anything
/// but one-time use.
pub fn fetch(rr: RequestRecord, tune: &Tunables) -> Result<Dialog, Flare> {
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
        .map_err(Flare::from)
        .and_then(|monolog| resp_future(monolog, tune))
        .and_then(|idialog| future::result(idialog.prepare()));

    // Run until completion
    core.run(work)
        .map_err(Flare::from)
}

/// Return a list of supported encodings from the headers Transfer-Encoding
/// and Content-Encoding.  The `Chunked` encoding will be the first value if
/// found. At most one compression encoding will be the last value if found.
pub fn find_encodings(headers: &http::HeaderMap)-> Vec<Encoding>
{
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
pub fn find_chunked(headers: &http::HeaderMap) -> bool
{
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

fn resp_future(monolog: Monolog, tune: &Tunables)
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

    let idialog = InDialog {
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        res_body:    bsink,
    };

    let tune = tune.clone();
    let s = body
        .map_err(Flare::from)
        .fold(idialog, move |mut idialog, chunk| {
            let new_len = idialog.res_body.len() + (chunk.len() as u64);
            if new_len > tune.max_body() {
                bail!("Response stream too long: {}+", new_len);
            } else {
                if idialog.res_body.is_ram() && new_len > tune.max_body_ram() {
                    idialog.res_body.write_back(tune.temp_dir())?;
                }
                debug!("to save chunk (len: {})", chunk.len());
                idialog.res_body
                    .save(chunk)
                    .and(Ok(idialog))
                    .map_err(Flare::from)
            }
        });
    Box::new(s)
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
        let req = create_request("https://www.usa.gov").unwrap();

        let dl = fetch(req, &tune).unwrap();
        let dl = dl.try_clone().unwrap();
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
