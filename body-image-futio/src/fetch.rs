use std::future::Future;

use bytes::Bytes;
use futures_util::{
    future::{Either, FutureExt as _, TryFutureExt},
    stream::{StreamExt, TryStreamExt},
};

use hyperx::header::{ContentLength, TypedHeaders};

use body_image::{BodySink, Dialog};

use crate::{
    AsyncBodySink, BlockingPolicy, DispatchBodySink, Flaw, FutioError,
    FutioTunables, InDialog, Monolog, PermitBodySink, RequestRecord,
    SinkWrapper,
};

/// Run an HTTP request to completion, returning the full `Dialog`. This
/// function constructs a tokio `Runtime` (ThreadPool),
/// `hyper_tls::HttpsConnector`, and `hyper::Client` in a simplistic form
/// internally, waiting with timeout, and dropping these on completion.
pub fn fetch<B>(rr: RequestRecord<B>, tune: FutioTunables)
    -> Result<Dialog, FutioError>
    where B: http_body::Body + Send + 'static,
          B::Data: Send + Unpin,
          B::Error: Into<Flaw>
{
    let mut rt = tokio::runtime::Builder::new()
        .core_threads(2)
        .max_threads(4)
        .threaded_scheduler()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    let connector = hyper_tls::HttpsConnector::new();
    let client = hyper::Client::builder().build(connector);

    let join = rt.spawn(request_dialog(&client, rr, tune));
    rt.block_on(join)
        .map_err(|e| FutioError::Other(Box::new(e)))?
}

/// Given a suitable `hyper::Client` and `RequestRecord`, return a
/// `Future<Output=Result<Dialog, FutioError>>.  The provided `FutioTunables`
/// governs timeout intervals (initial response and complete body) and if the
/// response `BodyImage` will be in `Ram` or `FsRead`.
pub fn request_dialog<CN, B>(
    client: &hyper::Client<CN, B>,
    rr: RequestRecord<B>,
    tune: FutioTunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send + 'static
    where CN: hyper::client::connect::Connect + Clone + Send + Sync + 'static,
          B: http_body::Body + Send + 'static,
          B::Data: Send,
          B::Error: Into<Flaw>
{
    let prolog = rr.prolog;

    let futr = client
        .request(rr.request)
        .err_into::<FutioError>()
        .map_ok(|response| Monolog { prolog, response });

    let futr = if let Some(t) = tune.res_timeout() {
        Either::Left(
            tokio::time::timeout(t, futr).map(move |r| match r {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(FutioError::ResponseTimeout(t))
            })
        )
    } else {
        Either::Right(futr)
    };

    async move {
        let monolog = futr .await?;

        let body_timeout = tune.body_timeout();

        let futr = resp_future(monolog, tune);

        let futr = if let Some(t) = body_timeout {
            Either::Left(
                tokio::time::timeout(t, futr).map(move |r| match r {
                    Ok(Ok(v)) => Ok(v),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(FutioError::BodyTimeout(t))
                })
            )
        } else {
            Either::Right(futr)
        };

        futr .await? .prepare()
    }
}

async fn resp_future(monolog: Monolog, tune: FutioTunables)
    -> Result<InDialog, FutioError>
{
    let (resp_parts, body) = monolog.response.into_parts();

    // Result<BodySink> based on CONTENT_LENGTH header.
    let bsink = match resp_parts.headers.try_decode::<ContentLength>() {
        Some(Ok(ContentLength(l))) => {
            if l > tune.image().max_body() {
                Err(FutioError::ContentLengthTooLong(l))
            } else if l > tune.image().max_body_ram() {
                BodySink::with_fs(tune.image().temp_dir())
                    .map_err(FutioError::from)
            } else {
                Ok(BodySink::with_ram(l))
            }
        },
        Some(Err(e)) => Err(FutioError::Other(Box::new(e))),
        None => Ok(BodySink::with_ram(tune.image().max_body_ram()))
    }?;

    // Regardless of policy, we always receive `Bytes` from hyper, so there is
    // no advantage to converting to `UniBodyBuf` here. Memory mapped buffers
    // are never received.

    let res_body = match tune.blocking_policy() {
        BlockingPolicy::Direct => {
            let mut sink = AsyncBodySink::<Bytes>::new(bsink, tune);
            body.err_into::<FutioError>()
                .forward(&mut sink)
                .await?;
            sink.into_inner()
        }
        BlockingPolicy::Permit(_) => {
            let mut sink = PermitBodySink::<Bytes>::new(bsink, tune);
            body.err_into::<FutioError>()
                .forward(&mut sink)
                .await?;
            sink.into_inner()
        }
        BlockingPolicy::Dispatch => {
            let mut sink = DispatchBodySink::<Bytes>::new(bsink, tune);
            body.err_into::<FutioError>()
                .forward(&mut sink)
                .await?;
            sink.into_inner()
        }
    };

    Ok(InDialog {
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        res_body
    })
}
