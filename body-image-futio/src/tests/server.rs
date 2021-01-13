use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::net::TcpListener as StdTcpListener;
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
use tao_log::{debug, debugv, info, warn};

use tokio::net::TcpListener;
use tokio::spawn;

use hyper::Body;
use hyper::client::{Client, HttpConnector};
use hyper::server::conn::Http;
use hyper::service::{make_service_fn, service_fn};

use body_image::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};

use crate::{
    AsyncBodyImage, AsyncBodySink,
    BlockingPolicy,
    Flaw, FutioError, FutioTunables, FutioTuner,
    RequestRecord, RequestRecorder,
    request_dialog, user_agent,
    UniBodyBuf
};

use piccolog::test_logger;

lazy_static! {
    static ref BLOCKING_TEST_SET: Semaphore = Semaphore::default_new(2);
}

// Return a tuple of (serv: impl Future, url: String) that will service C
// requests via function, and the url to access it via a local tcp port.
macro_rules! service {
    ($c:literal, $s:ident) => {{
        let (listener, addr) = local_bind().unwrap();
        let fut = async move {
            for i in 0..$c {
                info!("service! accepting...");
                let socket = listener.accept()
                    .await
                    .expect("accept").0;
                socket.set_nodelay(true).expect("nodelay");
                info!("service! accepted, serve...");
                let res = Http::new()
                    .serve_connection(socket, service_fn($s))
                    .await;
                if let Err(e) = res {
                    warn!("On service! [{}]: {}", i, e);
                    break;
                }
            }
            info!("service! completing");
        };
        (format!("http://{}", &addr), fut)
    }}
}

#[test]
fn post_echo_body() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = service!(1, echo);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_res_timeout(Duration::from_millis(10000))
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .finish();
        let body = fs_body_image(445);
        let res = spawn(post_body_req::<Body>(&url, body, tune))
            .await
            .unwrap();
        info!("dialog futr ready, awaiting service join...");
        let _ = jh .await;
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
        let (url, srv) = service!(1, echo_async);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .finish();
        let body = fs_body_image(445);
        let res = spawn(
            post_body_req::<AsyncBodyImage<Bytes>>(&url, body, tune)
        )
            .await
            .unwrap();
        let _ = jh .await;
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
        let (url, srv) = service!(20, echo_async);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .finish();
        let futures: FuturesUnordered<_> = (0..20).map(|i| {
            let body = fs_body_image(445 + i);
            spawn(post_body_req::<AsyncBodyImage<Bytes>>(
                &url, body, tune.clone()
            ))
        }).collect();
        let res = futures.collect::<Vec<_>>() .await;
        let _ = jh. await;
        res
    });
    assert_eq!(20, res.iter().filter(|r| r.is_ok()).count());
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_async_body_mmap_copy() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = service!(1, echo_async);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .finish();
        let mut body = fs_body_image(445);
        body.mem_map().unwrap();
        let res = spawn(
            post_body_req::<AsyncBodyImage<Bytes>>(&url, body, tune)
        )
            .await
            .unwrap();
        let _ = jh .await;
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
        let (url, srv) = service!(1, echo_uni_mmap);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(2048).finish())
            .finish();
        let body = fs_body_image(194_767);
        let res = spawn(post_body_req::<AsyncBodyImage<UniBodyBuf>>(
            &url, body, tune
        ))
            .await
            .unwrap();
        let _ = jh .await;
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
        let (url, srv) = service!(1, delayed);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_res_timeout(Duration::from_millis(10))
            .set_body_timeout(Duration::from_millis(600))
            .finish();
        let res = spawn(get_req::<AsyncBodyImage<Bytes>>(&url, tune))
            .await
            .unwrap();
        let _ = jh .await;
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
        let (url, srv) = service!(1, delayed);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .unset_res_timeout() // workaround, see *_race version of test below
            .set_body_timeout(Duration::from_millis(600))
            .finish();
        let res = spawn(get_req::<AsyncBodyImage<Bytes>>(&url, tune))
            .await
            .unwrap();
        let _ = jh .await;
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

#[cfg(feature = "may-fail")]
#[test]
fn timeout_during_streaming_race() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = service!(1, delayed);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            // Correct test assertion, but this may fail due to timing
            // issues
            .set_res_timeout(Duration::from_millis(590))
            .set_body_timeout(Duration::from_millis(600))
            .finish();
        let res = spawn(get_req::<AsyncBodyImage<Bytes>>(&url, tune))
            .await
            .unwrap();
        let _ = jh .await;
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
            .finish();
        let connector = HttpConnector::new();
        let client = Client::builder().build(connector);
        let futures: FuturesUnordered<_> = (0..20).map(|_| {
            let req: RequestRecord<AsyncBodyImage<Bytes>> = http::Request::builder()
                .method(http::Method::GET)
                .uri(&url)
                .record()
                .unwrap();
            spawn(request_dialog(&client, req, tune.clone()))
        }).collect();
        let res = futures.collect::<Vec<_>>() .await;

        // Graceful shutdown sequence (otherwise this will occasional hang)
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

fn local_bind() -> Result<(TcpListener, SocketAddr), io::Error> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let std_listener = StdTcpListener::bind(addr).unwrap();
    let listener = TcpListener::from_std(std_listener)?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
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

fn get_req<T>(url: &str, tune: FutioTunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send
    where T: http_body::Body + Send + 'static,
          T::Data: Send,
          T::Error: Into<Flaw>,
          http::request::Builder: RequestRecorder<T>
{
    let req: RequestRecord<T> = http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header(http::header::USER_AGENT, &user_agent()[..])
        .record()
        .unwrap();
    let connector = HttpConnector::new();
    let client: Client<_, T> = Client::builder().build(connector);
    request_dialog(&client, req, tune)
}

fn post_body_req<T>(url: &str, body: BodyImage, tune: FutioTunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send
    where T: http_body::Body + Send + 'static,
          T::Data: Send,
          T::Error: Into<Flaw>,
          http::request::Builder: RequestRecorder<T>
{
    let req: RequestRecord<T> = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .record_body_image(body, tune.clone())
        .unwrap();
    let connector = HttpConnector::new();
    let client: Client<_, T> = Client::builder().build(connector);
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
