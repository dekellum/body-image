//! HTTP client integration and utilities.

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
use self::hyper::header::{ContentEncoding, ContentLength, Encoding,
                          Header, Raw};
use self::tokio_core::reactor::Core;

use {BodyImage, BodySink,
     Prolog, Dialog, RequestRecorded, Tunables,
     META_URL, META_METHOD, META_RES_DECODED,
     META_RES_VERSION, META_RES_STATUS};

/// The HTTP request (with body) type (as of hyper 0.11.x.)
type HyRequest = http::Request<hyper::Body>;

/// Run an HTTP request to completion, returning the full `Dialog`.
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

/// Decode any _gzip_, _deflate_, or (optional feature) _brotli_
/// response Transfer-Encoding or Content-Encoding into a new response
/// `BodyImage`, updating `Dialog` accordingly. The provided `Tunables`
/// controls decompression buffer sizes and if the final `BodyImage`
/// will be in `Ram` or `FsRead`.
pub fn decode_res_body(dialog: &mut Dialog, tune: &Tunables)
    -> Result<(), Flare>
{
    let headers = &dialog.res_headers;
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
                    Encoding::Chunked => chunked = true,
                    Encoding::Gzip | Encoding::Deflate => { // supported
                        compress = Some(av.clone());
                        break 'headers;
                    }
                    #[cfg(feature = "brotli")]
                    Encoding::Brotli => {
                        compress = Some(Encoding::Brotli);
                        break 'headers;
                    }
                    Encoding::Identity => (),
                    _ => {
                        warn!("decode_res_body: Unsupported encoding: {:?}",
                              av);
                        break 'headers;
                    }
                }
            }
        }
    }

    if let Some(ref comp) = compress {
        dialog.res_body = {
            debug!("Body to {:?} decode: {:?}", comp, dialog.res_body);
            let mut reader = dialog.res_body.reader();
            match *comp {
                Encoding::Gzip => {
                    let mut decoder = GzDecoder::new(reader.as_read());
                    let len_est = dialog.res_body.len() *
                        u64::from(tune.size_estimate_gzip());
                    BodyImage::read_from(&mut decoder, len_est, tune)?
                }
                Encoding::Deflate => {
                    let mut decoder = DeflateDecoder::new(reader.as_read());
                    let len_est = dialog.res_body.len() *
                        u64::from(tune.size_estimate_deflate());
                    BodyImage::read_from(&mut decoder, len_est, tune)?
                }
                #[cfg(feature = "brotli")]
                Encoding::Brotli => {
                    let mut decoder = brotli::Decompressor::new(
                        reader.as_read(),
                        tune.buffer_size_ram());
                    let len_est = dialog.res_body.len() *
                        u64::from(tune.size_estimate_brotli());
                    BodyImage::read_from(&mut decoder, len_est, tune)?
                }
                _ => unreachable!("Not supported: {:?}", comp)
            }
        };
        debug!("Body update: {:?}", dialog.res_body);
    }

    if chunked || compress.is_some() {
        let mut ds = Vec::with_capacity(2);
        if chunked {
            ds.push(Encoding::Chunked.to_string())
        }
        if let Some(ref e) = compress {
            ds.push(e.to_string())
        }
        dialog.meta.append(http::header::HeaderName
                           ::from_lowercase(META_RES_DECODED).unwrap(),
                           ds.join(", ").parse()?);
    }
    Ok(())
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

/// An `http::Request` and recording.
#[derive(Debug)]
pub struct RequestRecord {
    request:      HyRequest,
    prolog:       Prolog,
}

impl RequestRecord {
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
    /// Prepare the response body for reading and generate meta
    /// headers.
    fn prepare(self) -> Result<Dialog, Flare> {
        Ok(Dialog {
            meta:        self.derive_meta()?,
            prolog:      self.prolog,
            version:     self.version,
            status:      self.status,
            res_headers: self.res_headers,
            res_body:    self.res_body.prepare()?,
        })
    }

    fn derive_meta(&self) -> Result<http::HeaderMap, Flare> {
        let mut hs = http::HeaderMap::with_capacity(6);
        use http::header::HeaderName;

        hs.append(HeaderName::from_lowercase(META_URL).unwrap(),
                  self.prolog.url.to_string().parse()?);
        hs.append(HeaderName::from_lowercase(META_METHOD).unwrap(),
                  self.prolog.method.to_string().parse()?);

        // FIXME: This relies on the debug format of version,  e.g. "HTTP/1.1"
        // which might not be stable, but http::Version doesn't offer an enum
        // to match on, only constants.
        let v = format!("{:?}", self.version);
        hs.append(HeaderName::from_lowercase(META_RES_VERSION).unwrap(),
                  v.parse()?);

        hs.append(HeaderName::from_lowercase(META_RES_STATUS).unwrap(),
                  self.status.to_string().parse()?);
        Ok(hs)
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
            .header(http::header::ACCEPT,
                    "text/html, application/xhtml+xml, \
                     application/xml;q=0.9, \
                     */*;q=0.8" )
            .header(http::header::ACCEPT_LANGUAGE, "en")
            .header(http::header::ACCEPT_ENCODING, "br, gzip, deflate")
            .header(http::header::USER_AGENT,
                    "Mozilla/5.0 \
                     (compatible; body-image 0.1.0; \
                      +http://github.com/dekellum/body-image)")
            // Referer? Etag, If-Modified...?
            // "Connection: keep-alive" (header) is default for HTTP 1.1
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
