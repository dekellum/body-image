use std::future::Future;

use futures::{
    future::{Either, FutureExt as _, TryFutureExt},
    stream::{StreamExt, TryStreamExt},
};

use hyperx::header::{ContentLength, TypedHeaders};

use body_image::{BodySink, Dialog, Tunables};

use tokio::future::FutureExt;

use crate::{
    AsyncBodySink, InDialog, FutioError, Monolog, RequestRecord
};

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
