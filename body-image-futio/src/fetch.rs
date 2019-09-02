use std::fmt;
use std::future::Future;

use futures::{
    future::{Either, FutureExt as _, TryFutureExt},
    stream::{StreamExt, TryStreamExt},
};
use hyperx::header::{ContentLength, TypedHeaders};
use tokio::future::FutureExt;
use tokio::runtime::{Runtime as ThRuntime, Builder as ThBuilder};
use tokio::runtime::current_thread::Runtime as CtRuntime;

use body_image::{BodySink, Dialog, Tunables};

use crate::{
    AsyncBodySink, InDialog, FutioError, Monolog, RequestRecord, SinkWrapper,
};

/// Extension trait for various kinds of runtimes.
pub trait RuntimeExt {
    /// Like Runtime::block_on but blocks on a oneshot receiver, with the
    /// original future spawned, to ensure it actually runs on a worker thread.
    fn block_on_pool<F>(&mut self, futr: F) -> F::Output
        where F: Future + Send + 'static,
              F::Output: fmt::Debug + Send;
}

impl RuntimeExt for ThRuntime {
    // FIXME: Not sure if this is intended behavior for tokio 0.2 Runtime
    fn block_on_pool<F>(&mut self, futr: F) -> F::Output
        where F: Future + Send + 'static,
              F::Output: fmt::Debug + Send
    {
        let (tx, rx) = futures::channel::oneshot::channel();
        self.spawn(futr.map(move |o| tx.send(o).expect("send")));
        self.block_on(rx).expect("recv")
    }
}

impl RuntimeExt for CtRuntime {
    fn block_on_pool<F>(&mut self, futr: F) -> F::Output
        where F: Future
    {
        self.block_on(futr)
    }
}

/// Run an HTTP request to completion, returning the full `Dialog`. This
/// function constructs a tokio `Runtime` (ThreadPool),
/// `hyper_tls::HttpsConnector`, and `hyper::Client` in a simplistic form
/// internally, waiting with timeout, and dropping these on completion.
pub fn fetch<B>(rr: RequestRecord<B>, tune: &Tunables)
    -> Result<Dialog, FutioError>
    where B: hyper::body::Payload + Send + Unpin,
          <B as hyper::body::Payload>::Data: Unpin
{
    let mut rt = ThBuilder::new()
        .name_prefix("tpool-")
        .core_threads(2)
        .blocking_threads(1024)
        .build()
        .unwrap();

    let res = {
        let connector = hyper_tls::HttpsConnector::new(1 /*DNS threads*/)
            .map_err(|e| FutioError::Other(Box::new(e)))?;
        let client = hyper::Client::builder().build(connector);

        rt.block_on_pool(request_dialog(&client, rr, tune))
    };

    rt.shutdown_now();
    res
}

/// Given a suitable `hyper::Client` and `RequestRecord`, return a
/// `Future<Output=Result<Dialog, FutioError>>.  The provided `Tunables`
/// governs timeout intervals (initial response and complete body) and if the
/// response `BodyImage` will be in `Ram` or `FsRead`.
pub fn request_dialog<CN, B>(
    client: &hyper::Client<CN, B>,
    rr: RequestRecord<B>,
    tune: &Tunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send + 'static
    where CN: hyper::client::connect::Connect + Sync + 'static,
          B: hyper::body::Payload + Send + Unpin,
          <B as hyper::body::Payload>::Data: Unpin
{
    let prolog = rr.prolog;

    let futr = client
        .request(rr.request)
        .err_into::<FutioError>()
        .map_ok(|response| Monolog { prolog, response });

    let futr = if let Some(t) = tune.res_timeout() {
        Either::Left(
            futr.timeout(t).map(move |r| match r {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(FutioError::ResponseTimeout(t))
            })
        )
    } else {
        Either::Right(futr)
    };

    let tune = tune.clone();

    async move {
        let monolog = futr .await?;

        let body_timeout = tune.body_timeout();

        let futr = resp_future(monolog, tune);

        let futr = if let Some(t) = body_timeout {
            Either::Left(
                futr.timeout(t).map(move |r| match r {
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

async fn resp_future(monolog: Monolog, tune: Tunables)
    -> Result<InDialog, FutioError>
{
    let (resp_parts, body) = monolog.response.into_parts();

    // Result<BodySink> based on CONTENT_LENGTH header.
    let bsink = match resp_parts.headers.try_decode::<ContentLength>() {
        Some(Ok(ContentLength(l))) => {
            if l > tune.max_body() {
                Err(FutioError::ContentLengthTooLong(l))
            } else if l > tune.max_body_ram() {
                BodySink::with_fs(tune.temp_dir()).map_err(FutioError::from)
            } else {
                Ok(BodySink::with_ram(l))
            }
        },
        Some(Err(e)) => Err(FutioError::Other(Box::new(e))),
        None => Ok(BodySink::with_ram(tune.max_body_ram()))
    }?;

    let mut async_body = AsyncBodySink::new(bsink, tune);

    body.err_into::<FutioError>()
        .forward(&mut async_body)
        .await?;

    Ok(InDialog {
        prolog:      monolog.prolog,
        version:     resp_parts.version,
        status:      resp_parts.status,
        res_headers: resp_parts.headers,
        res_body:    async_body.into_inner()
    })
}
