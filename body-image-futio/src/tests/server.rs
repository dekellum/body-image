use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::net::TcpListener as StdTcpListener;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::future::{FutureExt, TryFutureExt};
use futures::stream::{StreamExt, TryStreamExt};
use http::{Request, Response};
use http;
use tao_log::{debug, debugv, warn};

use tokio;
use tokio::runtime::{Runtime as ThRuntime, Builder as ThBuilder};
use tokio_net::driver::Handle;
use tokio_net::tcp::TcpListener;

use hyper;
use hyper::Body;
use hyper::client::{Client, HttpConnector};
use hyper::server::conn::Http;
use hyper::service::service_fn;

use body_image::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};

use crate::{AsyncBodyImage, FutioError, RequestRecord, RequestRecorder, RuntimeExt,
            request_dialog, user_agent};
#[cfg(feature = "mmap")] use crate::{AsyncBodySink, UniBodyImage};
use crate::logger::test_logger;

macro_rules! one_service {
    ($s:ident) => {{
        let (listener, addr) = local_bind().unwrap();
        let fut = async move {
            let mut incoming = listener.incoming();
            let socket = incoming.next()
                .await
                .expect("some")
                .expect("socket");
            socket.set_nodelay(true).expect("nodelay");
            Http::new()
                .serve_connection(socket, service_fn($s))
                .await
        };
        let fut = fut
            .map_err(|e| warn!("On one_service connection: {}", e))
            .map(|_| ());
        (fut, format!("http://{}", &addr))
    }}
}

#[test]
fn post_echo_body() {
    assert!(test_logger());
    let mut rt = new_limited_runtime();

    let (serv, url) = one_service!(echo);
    rt.spawn(serv);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image(445);
    let client = post_body_req::<Body>(&url, body, &tune);
    match rt.block_on_pool(client) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle();
}

#[test]
fn post_echo_async_body() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (serv, url) = one_service!(echo);
    rt.spawn(serv);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image(445);
    let client = post_body_req::<AsyncBodyImage>(&url, body, &tune);
    match rt.block_on_pool(client) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle();
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_async_body_mmap_copy() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = one_service!(echo);
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let mut body = fs_body_image(445);
    body.mem_map().unwrap();
    let client = post_body_req::<AsyncBodyImage>(&url, body, &tune);
    match rt.block_on_pool(client) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle();
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_uni_body_mmap() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = one_service!(echo_uni_mmap);
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(2048)
        .finish();
    let body = fs_body_image(194_767);
    match rt.block_on_pool(post_body_req::<UniBodyImage>(&url, body, &tune)) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 194_767);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle();
}

#[test]
fn timeout_before_response() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = one_service!(delayed);
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_res_timeout(Duration::from_millis(10))
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    match rt.block_on_pool(get_req::<AsyncBodyImage>(&url, &tune)) {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::ResponseTimeout(_) => debug!("expected: {}", e),
            _ => panic!("not response timeout {:?}", e),
        }
    }
    rt.shutdown_on_idle();
}

#[test]
fn timeout_during_streaming() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = one_service!(delayed);
    rt.spawn(fut);

    let tune = Tuner::new()
        .unset_res_timeout() // workaround, see *_race version of test below
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    match rt.block_on_pool(get_req::<AsyncBodyImage>(&url, &tune)) {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::BodyTimeout(_) => debug!("expected: {}", e),
            _ => panic!("not a body timeout {:?}", e),
        }
    }
    rt.shutdown_on_idle();
}

#[test]
#[cfg(feature = "may_fail")]
fn timeout_during_streaming_race() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = one_service!(delayed);
    rt.spawn(fut);

    let tune = Tuner::new()
        // Correct test assertion, but this may fail on CI due to timing
        // issues
        .set_res_timeout(Duration::from_millis(590))
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    match rt.block_on_pool(get_req::<AsyncBodyImage>(&url, &tune)) {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::BodyTimeout(_) => {},
            _ => panic!("not a body timeout {:?}", e),
        }
    }
    rt.shutdown_on_idle();
}

async fn echo(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    Ok(debugv!("echo", Response::new(req.into_body())))
}

async fn echo_uni_mmap(req: Request<Body>)
    -> Result<Response<UniBodyImage>, FutioError>
{
    let tune = Tuner::new()
        .set_buffer_size_fs(2734)
        .set_max_body_ram(15_000)
        .finish();
    let mut asink = AsyncBodySink::new(
        BodySink::with_ram_buffers(4),
        tune
    );
    req.into_body()
        .err_into::<FutioError>()
        .forward(&mut asink)
        .await?;

    let tune = Tuner::new().set_buffer_size_fs(4972).finish();
    let mut bi = asink.into_inner().prepare()?;
    bi.mem_map()?;

    Ok(Response::builder()
       .status(200)
       .body(debugv!("echo server (mmap)", UniBodyImage::new(bi, &tune)))
       .expect("response"))
}

async fn delayed(_req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    let bi = ram_body_image(0x8000, 32);
    let tune = Tunables::default();
    let now = Instant::now();
    let delay1 = tokio::timer::delay(now + Duration::from_millis(100));
    let delay2 = tokio::timer::delay(now + Duration::from_millis(900));
    delay1.await;
    let rbody = AsyncBodyImage::new(bi, &tune)
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
    let listener = TcpListener::from_std(std_listener, &Handle::default())?;
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

fn get_req<T>(url: &str, tune: &Tunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send
    where T: hyper::body::Payload + Send + Unpin,
          <T as hyper::body::Payload>::Data: Unpin,
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
    request_dialog(&client, req, &tune)
}

fn post_body_req<T>(url: &str, body: BodyImage, tune: &Tunables)
    -> impl Future<Output=Result<Dialog, FutioError>> + Send
    where T: hyper::body::Payload + Send + Unpin,
          <T as hyper::body::Payload>::Data: Unpin,
          http::request::Builder: RequestRecorder<T>
{
    let req: RequestRecord<T> = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .record_body_image(body, &tune)
        .unwrap();
    let connector = HttpConnector::new();
    let client: Client<_, T> = Client::builder().build(connector);
    request_dialog(&client, req, &tune)
}

fn new_limited_runtime() -> ThRuntime {
    ThBuilder::new()
        .name_prefix("tpool-")
        .core_threads(2)
        .blocking_threads(2) // FIXME: probably need to increase
        .build()
        .expect("runtime build")
}
