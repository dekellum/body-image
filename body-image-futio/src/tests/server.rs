use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::net::TcpListener as StdTcpListener;
use std::time::Duration;

use blocking_permit::Semaphore;
use bytes::Bytes;
use futures_util::{
    future::FutureExt,
    stream::{FuturesUnordered, StreamExt, TryStreamExt}
};
use http::{Request, Response};
use lazy_static::lazy_static;
use tao_log::{debug, debugv, warn};

use tokio::net::TcpListener;
use tokio::spawn;

use hyper::Body;
use hyper::client::{Client, HttpConnector};
use hyper::server::conn::Http;
use hyper::service::service_fn;

#[cfg(feature = "may_fail")]
use hyper::service::make_service_fn;

#[cfg(feature = "may_fail")]
use futures_util::future::TryFutureExt;

use body_image::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};

use crate::{
    AsyncBodyImage, AsyncBodySink,
    Flaw, FutioError, FutioTunables, FutioTuner,
    RequestRecord, RequestRecorder,
    request_dialog, user_agent
};
#[cfg(feature = "mmap")] use crate::UniBodyImage;
use crate::logger::test_logger;

lazy_static! {
    static ref BLOCKING_TEST_SET: Semaphore = Semaphore::new(true, 2);
}

// Return a tuple of (serv: impl Future, url: String) where serv will service a
// single request via the passed function, and the url to access it via a local
// tcp port.
macro_rules! service {
    ($c:literal, $s:ident) => {{
        let (mut listener, addr) = local_bind().unwrap();
        let fut = async move {
            for i in 0..$c {
                let mut incoming = listener.incoming();
                let socket = incoming.next()
                    .await
                    .expect("some")
                    .expect("socket");
                socket.set_nodelay(true).expect("nodelay");
                let res = Http::new()
                    .serve_connection(socket, service_fn($s))
                    .await;
                if let Err(e) = res {
                    warn!("On service! [{}]: {}", i, e);
                    break;
                }
            }
        };
        (format!("http://{}", &addr), fut)
    }}
}

// Like above, but return tuple of persistent server and its url
#[cfg(feature = "may_fail")]
macro_rules! server {
    ($s:ident) => {{
        let server = hyper::Server::bind(&([127, 0, 0, 1], 0).into())
            .serve(make_service_fn(|_| async {
                Ok::<_, hyper::Error>(service_fn($s))
            }));
        let local_addr = format!("http://{}", server.local_addr());
        let fut = server
            .map_err(|e| warn!("On server: {}", e))
            .map(|_| ());
        (local_addr, fut)
    }}
}

#[test]
fn post_echo_body() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = service!(1, echo);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let body = fs_body_image(445);
        let res = spawn(post_body_req::<Body>(&url, body, tune))
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
fn post_echo_async_body() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = service!(1, echo_async);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let body = fs_body_image(445);
        let res = spawn(post_body_req::<AsyncBodyImage>(&url, body, tune))
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

// FIXME: Unreliable due to lack of well timed server shutdown?
#[cfg(feature = "may_fail")]
#[test]
fn post_echo_async_body_multi_server() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = server!(echo_async);
        spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let futures: FuturesUnordered<_> = (0..20).map(|i| {
            let body = fs_body_image(445 + i);
            spawn(post_body_req::<AsyncBodyImage>(&url, body, tune.clone()))
        }).collect();
        futures.collect::<Vec<_>>() .await
    });
    assert_eq!(20, res.iter().filter(|r| r.is_ok()).count());
}

#[test]
fn post_echo_async_body_multi() {
    assert!(test_logger());
    let res = new_limited_runtime().block_on(async {
        let (url, srv) = service!(20, echo_async);
        let jh = spawn(srv);
        let tune = FutioTuner::new()
            .set_image(Tuner::new().set_buffer_size_fs(17).finish())
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let futures: FuturesUnordered<_> = (0..20).map(|i| {
            let body = fs_body_image(445 + i);
            spawn(post_body_req::<AsyncBodyImage>(&url, body, tune.clone()))
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
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let mut body = fs_body_image(445);
        body.mem_map().unwrap();
        let res = spawn(post_body_req::<AsyncBodyImage>(&url, body, tune))
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
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let body = fs_body_image(194_767);
        let res = spawn(post_body_req::<UniBodyImage>(&url, body, tune))
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
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let res = spawn(get_req::<AsyncBodyImage>(&url, tune))
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
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let res = spawn(get_req::<AsyncBodyImage>(&url, tune))
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

#[test]
#[cfg(feature = "may_fail")]
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
            .set_blocking_semaphore(&BLOCKING_TEST_SET)
            .finish();
        let res = spawn(get_req::<AsyncBodyImage>(&url, tune))
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

async fn echo(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    Ok(debugv!("echo", Response::new(req.into_body())))
}

async fn echo_async(req: Request<Body>)
    -> Result<Response<AsyncBodyImage>, FutioError>
{
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_buffer_size_fs(2734)
                .set_max_body_ram(15_000)
                .finish()
        )
        .set_blocking_semaphore(&BLOCKING_TEST_SET)
        .finish();
    let mut asink = AsyncBodySink::new(
        BodySink::with_ram_buffers(4),
        tune
    );
    req.into_body()
        .err_into::<FutioError>()
        .forward(&mut asink)
        .await?;

    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(4972).finish())
        .set_blocking_semaphore(&BLOCKING_TEST_SET)
        .finish();
    let bi = asink.into_inner().prepare()?;

    Ok(Response::builder()
       .status(200)
       .body(debugv!("echo (async)", AsyncBodyImage::new(bi, tune)))
       .expect("response"))
}

#[cfg(feature = "mmap")]
async fn echo_uni_mmap(req: Request<Body>)
    -> Result<Response<UniBodyImage>, FutioError>
{
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_buffer_size_fs(2734)
                .set_max_body_ram(15_000)
                .finish()
        )
        .set_blocking_semaphore(&BLOCKING_TEST_SET)
        .finish();
    let mut asink = AsyncBodySink::new(
        BodySink::with_ram_buffers(4),
        tune
    );
    req.into_body()
        .err_into::<FutioError>()
        .forward(&mut asink)
        .await?;

    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(4972).finish())
        .set_blocking_semaphore(&BLOCKING_TEST_SET)
        .finish();
    let mut bi = asink.into_inner().prepare()?;
    bi.mem_map()?;

    Ok(Response::builder()
       .status(200)
       .body(debugv!("echo (mmap)", UniBodyImage::new(bi, tune)))
       .expect("response"))
}

async fn delayed(_req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let bi = ram_body_image(0x8000, 32);
    let tune = FutioTunables::default();
    let now = tokio::time::Instant::now();
    let delay1 = tokio::time::delay_until(now + Duration::from_millis(100));
    let delay2 = tokio::time::delay_until(now + Duration::from_millis(900));
    delay1.await;
    let rbody = AsyncBodyImage::new(bi, tune)
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
        bs.save(vec![1; csize]).expect("safe for Ram");
    }
    bs.prepare().expect("safe for Ram")
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
    tokio::runtime::Builder::new()
        .num_threads(2)
        .threaded_scheduler()
        .enable_io()
        .enable_time()
        .build()
        .expect("limited_runtime build")
}
