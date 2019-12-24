use bytes::Bytes;
use futures_core::stream::Stream;
use futures_sink::Sink;

use body_image::{BodyImage, BodySink, Prolog};

use crate::{AsyncBodyImage, FutioTunables, RequestRecord};

/// Trait for generic construction of `Stream` wrapper types.
pub trait StreamWrapper: Stream {
    /// Wrap by consuming the `BodyImage` instance.
    ///
    /// *Note*: `BodyImage` and `FutioTunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    fn new(body: BodyImage, tune: FutioTunables) -> Self;
}

/// Trait for generic construction of `Sink` wrapper types.
pub trait SinkWrapper<T>: Sink<T> {
    /// Wrap by consuming a `BodySink` and `FutioTunables` instances.
    ///
    /// *Note*: `FutioTunables` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    fn new(body: BodySink, tune: FutioTunables) -> Self;

    /// Unwrap and return the `BodySink`.
    ///
    /// ## Panics
    ///
    /// May panic if called after a `Result::Err` is returned from any `Sink`
    /// method or before `Sink::poll_flush` or `Sink::poll_close` is called.
    fn into_inner(self) -> BodySink;
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
            let stream = AsyncBodyImage::<Bytes>::new(body.clone(), tune);
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

impl<SW> RequestRecorder<SW> for http::request::Builder
    where SW: StreamWrapper + http_body::Body + Send
{
    fn record(self) -> Result<RequestRecord<SW>, http::Error> {
        let request = {
            let body = BodyImage::empty();
            // Tunables are unused for empty body, so default is sufficient.
            let tune = FutioTunables::default();
            self.body(SW::new(body, tune))?
        };
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        let req_body = BodyImage::empty();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body }
        })
    }

    fn record_body<BB>(self, body: BB) -> Result<RequestRecord<SW>, http::Error>
        where BB: Into<Bytes>
    {
        let buf: Bytes = body.into();
        let req_body = if buf.is_empty() {
            BodyImage::empty()
        } else {
            BodyImage::from_slice(buf)
        };
        // Tunables are unused for Ram based body, so default is sufficient.
        let tune = FutioTunables::default();
        let request = self.body(SW::new(req_body.clone(), tune))?;

        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body } })
    }

    fn record_body_image(self, body: BodyImage, tune: FutioTunables)
        -> Result<RequestRecord<SW>, http::Error>
    {
        let request = self.body(SW::new(body.clone(), tune))?;
        let method      = request.method().clone();
        let url         = request.uri().clone();
        let req_headers = request.headers().clone();

        Ok(RequestRecord {
            request,
            prolog: Prolog { method, url, req_headers, req_body: body } })
    }
}
