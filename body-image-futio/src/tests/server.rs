use std::future::Future;
use std::time::Duration;

use blocking_permit::{Semaphore, Semaphorish};
use bytes::Bytes;
use futures_util::{
    future,
    future::FutureExt,
    stream::{FuturesUnordered, StreamExt, TryStreamExt}
};
use http::{Request, Response};
use lazy_static::lazy_static;
use tao_log::{debug, debugv, warn};

use tokio::spawn;

use hyper::Body;
use hyper::client::{Client, HttpConnector};
use hyper::service::{make_service_fn, service_fn};

use body_image::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};

use crate::{
    AsyncBodyImage, AsyncBodySink,
    BlockingPolicy,
    Flaw, FutioError, FutioTunables, FutioTuner,
    RequestRecord, RequestRecorder,
    StreamWrapper,
    request_dialog, user_agent,
    UniBodyBuf
};

use piccolog::test_logger;

lazy_static! {
    static ref BLOCKING_TEST_SET: Semaphore = Semaphore::default_new(2);
}

#[test]
fn post_echo_body() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(echo);
        let client = Client::builder().build(HttpConnector::new());
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .set_res_timeout(Duration::from_millis(8000))
            .set_body_timeout(Duration::from_millis(10000))
            .finish();
        let body = fs_body_image(445);

        let res = spawn(
            post_dialog::<AsyncBodyImage<Bytes>>(&client, &url, body, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn post_echo_async_body() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(echo_async);
        let client = Client::builder().build(HttpConnector::new());
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .set_res_timeout(Duration::from_millis(8000))
            .set_body_timeout(Duration::from_millis(10000))
            .finish();
        let body = fs_body_image(445);

        let res = spawn(
            post_dialog::<AsyncBodyImage<Bytes>>(&client, &url, body, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn post_echo_async_body_multi() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(echo_async);
        let client = Client::builder().build(HttpConnector::new());
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .set_res_timeout(Duration::from_millis(8000))
            .set_body_timeout(Duration::from_millis(10000))
            .finish();
        let futures: FuturesUnordered<_> = (0..20).map(|i| {
            let body = fs_body_image(445 + i);
            spawn(post_dialog::<AsyncBodyImage<Bytes>>(
                &client, &url, body, tune.clone()
            ))
        }).collect();
        let res = futures.collect::<Vec<_>>() .await;

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    assert_eq!(20, res.iter().filter(|r| r.is_ok()).count());
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_async_body_mmap_copy() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(echo_async);
        let client = Client::builder().build(HttpConnector::new());

        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .set_res_timeout(Duration::from_millis(8000))
            .set_body_timeout(Duration::from_millis(10000))
            .finish();
        let mut body = fs_body_image(445);
        body.mem_map().unwrap();
        let res = spawn(
            post_dialog::<AsyncBodyImage<Bytes>>(&client, &url, body, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_uni_body_mmap() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(echo_uni_mmap);

        let client = Client::builder().build(HttpConnector::new());

        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(2048).finish())
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .set_res_timeout(Duration::from_millis(8000))
            .set_body_timeout(Duration::from_millis(10000))
            .finish();
        let body = fs_body_image(194_767);

        let res = spawn(
            post_dialog::<AsyncBodyImage<UniBodyBuf>>(&client, &url, body, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 194_767);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn timeout_before_response() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(delayed);

        let client = Client::builder().build(HttpConnector::new());

        let tune = FutioTuner::new()
            .set_res_timeout(Duration::from_millis(10))
            .set_body_timeout(Duration::from_millis(600))
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .finish();

        let res = spawn(
            get_dialog::<AsyncBodyImage<Bytes>>(&client, &url, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::ResponseTimeout(_) => debug!("expected: {}", e),
            _ => panic!("not response timeout {:?}", e),
        }
    }
}

#[test]
fn timeout_during_streaming() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(delayed);

        let client = Client::builder().build(HttpConnector::new());

        let tune = FutioTuner::new()
            .unset_res_timeout() // workaround, see *_race version of test below
            .set_body_timeout(Duration::from_millis(600))
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .finish();

        let res = spawn(
            get_dialog::<AsyncBodyImage<Bytes>>(&client, &url, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::BodyTimeout(_) => debug!("expected: {}", e),
            _ => panic!("not a body timeout {:?}", e),
        }
    }
}

#[test]
#[cfg(feature = "may-fail")]
fn timeout_during_streaming_race() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = server(delayed);

        let client = Client::builder().build(HttpConnector::new());

        let tune = FutioTuner::new()
            // Correct test assertion, but this may fail due to timing issues
            .set_res_timeout(Duration::from_millis(590))
            .set_body_timeout(Duration::from_millis(600))
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .finish();

        let res = spawn(
            get_dialog::<AsyncBodyImage<Bytes>>(&client, &url, tune)
        )
            .await
            .unwrap();

        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;
        res
    });
    match res {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::BodyTimeout(_) => {},
            _ => panic!("not a body timeout {:?}", e),
        }
    }
}

#[test]
fn get_async_body_multi_server() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, shutdown_tx, srv_jh) = body_server(
            ram_body_image(0x8000, 32),
            FutioTunables::default()
        );
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_max_body_ram(0x8000 * 33).finish())
            .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
            .set_res_timeout(Duration::from_millis(8000))
            .set_body_timeout(Duration::from_millis(10000))
            .finish();
        let client = Client::builder().build(HttpConnector::new());
        let futures: FuturesUnordered<_> = (0..20).map(|_| {
            spawn(get_dialog::<AsyncBodyImage<Bytes>>(&client, &url, tune.clone()))
        }).collect();
        let res = futures.collect::<Vec<_>>() .await;

        // Graceful shutdown sequence (otherwise this would occasionally hang)
        drop(client);
        shutdown_tx.send(()).unwrap();
        let _ = srv_jh .await;

        res
    });
    assert_eq!(20, res.len());
    for r in res {
        match r {
            Ok(Ok(dialog)) => {
                assert!(dialog.res_body().is_ram());
                assert_eq!(dialog.res_body().len(), 0x8000 * 32);
            }
            Ok(Err(e)) => {
                panic!("dialog failed with: {}", e);
            }
            Err(e) => {
                panic!("join failed with: {}", e);
            }
        }
    }
}

async fn echo(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    Ok(debugv!("echo", Response::new(req.into_body())))
}

async fn echo_async(req: Request<Body>)
    -> Result<Response<AsyncBodyImage<Bytes>>, FutioError>
{
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_buffer_size_fs(2734)
                .set_max_body_ram(15_000)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .set_res_timeout(Duration::from_millis(8000))
        .set_body_timeout(Duration::from_millis(10000))
        .finish();
    let mut asink = AsyncBodySink::<Bytes>::new(
        BodySink::with_ram_buffers(4),
        tune
    );
    req.into_body()
        .err_into::<FutioError>()
        .forward(&mut asink)
        .await?;

    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(4972).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .set_res_timeout(Duration::from_millis(8000))
        .set_body_timeout(Duration::from_millis(10000))
        .finish();
    let bi = asink.into_inner().prepare()?;

    Ok(Response::builder()
       .status(200)
       .body(debugv!("echo (async)", AsyncBodyImage::new(bi, tune)))
       .expect("response"))
}

#[cfg(feature = "mmap")]
async fn echo_uni_mmap(req: Request<Body>)
    -> Result<Response<AsyncBodyImage<UniBodyBuf>>, FutioError>
{
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_buffer_size_fs(2734)
                .set_max_body_ram(15_000)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .set_res_timeout(Duration::from_millis(8000))
        .set_body_timeout(Duration::from_millis(10000))
        .finish();
    let mut asink = AsyncBodySink::<Bytes>::new(
        BodySink::with_ram_buffers(4),
        tune
    );
    req.into_body()
        .err_into::<FutioError>()
        .forward(&mut asink)
        .await?;

    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(4972).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .set_res_timeout(Duration::from_millis(8000))
        .set_body_timeout(Duration::from_millis(10000))
        .finish();
    let mut bi = asink.into_inner().prepare()?;
    bi.mem_map()?;

    Ok(Response::builder()
       .status(200)
       .body(debugv!(
           "echo (mmap)",
           AsyncBodyImage::<UniBodyBuf>::new(bi, tune)
       ))
       .expect("response"))
}

async fn delayed(_req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let bi = ram_body_image(0x8000, 32);
    let tune = FutioTunables::default();
    let now = tokio::time::Instant::now();
    let delay1 = tokio::time::sleep_until(now + Duration::from_millis(100));
    let delay2 = tokio::time::sleep_until(now + Duration::from_millis(900));
    delay1 .await;
    let rbody = AsyncBodyImage::<Bytes>::new(bi, tune)
        .chain(
            delay2
                .map(|_| Ok(Bytes::new()))
                .into_stream()
        );
    Ok(Response::builder()
       .status(200)
       .body(Body::wrap_stream(rbody))
       .expect("response"))
}

fn fs_body_image(size: usize) -> BodyImage {
    let tune = Tunables::default();
    let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
    body.write_all(vec![1; size]).unwrap();
    body.prepare().unwrap()
}

fn ram_body_image(csize: usize, count: usize) -> BodyImage {
    let mut bs = BodySink::with_ram_buffers(count);
    for _ in 0..count {
        bs.push(vec![1; csize]).expect("safe for Ram");
    }
    bs.prepare().expect("safe for Ram")
}

fn body_server(body: BodyImage, tune: FutioTunables)
    -> (String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), hyper::Error>>)
{
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = hyper::Server::bind(&([127, 0, 0, 1], 0).into())
        .serve(make_service_fn(move |_| {
            let body = body.clone();
            let tune = tune.clone();
            future::ok::<_, FutioError>(service_fn( move |_req| {
                future::ok::<_, FutioError>(
                    Response::builder()
                        .status(200)
                        .body(AsyncBodyImage::<UniBodyBuf>::new(
                            body.clone(), tune.clone()
                        ))
                        .expect("response")
                )
            }))
        }));
    let local_addr = format!("http://{}", server.local_addr()).to_owned();
    let server = server
        .with_graceful_shutdown(async {
            rx .await
                .map_err(|e| warn!("On shutdown: {}", e))
                .ok();
        });
    let jh = spawn(server);
    (local_addr, tx, jh)
}

fn server<F, S, RB, RE>(f: F)
    -> (String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), hyper::Error>>)
    where F: Fn(Request<hyper::Body>) -> S + Copy + Send + Sync + 'static,
          S: Future<Output = Result<Response<RB>, RE>> + Send + 'static,
          RB: http_body::Body + Send + Sync + 'static,
          RB::Data: Send,
          RB::Error: Into<Flaw>,
          RE: Into<Flaw>
{
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = hyper::Server::bind(&([127, 0, 0, 1], 0).into())
        .serve(make_service_fn(move |_| {
            future::ok::<_, FutioError>(service_fn(f))
        }));
    let local_addr = format!("http://{}", server.local_addr()).to_owned();
    let server = server
        .with_graceful_shutdown(async {
            rx .await
                .map_err(|e| warn!("On shutdown: {}", e))
                .ok();
        });
    let jh = spawn(server);
    (local_addr, tx, jh)
}

fn get_dialog<B>(
    client: &hyper::Client<HttpConnector, B>,
    url: &str,
    tune: FutioTunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send + 'static
    where B: StreamWrapper + http_body::Body + Send + 'static,
          B::Data: Send,
          B::Error: Into<Flaw>
{
    let req: RequestRecord<B> = http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header(http::header::USER_AGENT, &user_agent()[..])
        .record()
        .unwrap();
    request_dialog(&client, req, tune)
}

fn post_dialog<B>(
    client: &hyper::Client<HttpConnector, B>,
    url: &str,
    body: BodyImage,
    tune: FutioTunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send + 'static
    where B: StreamWrapper + http_body::Body + Send + 'static,
          B::Data: Send,
          B::Error: Into<Flaw>
{
    let req: RequestRecord<B> = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .record_body_image(body, tune.clone())
        .unwrap();
    request_dialog(&client, req, tune)
}

fn new_limited_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_io()
        .enable_time()
        .build()
        .expect("limited_runtime build")
}
