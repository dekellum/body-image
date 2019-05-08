use std::net::SocketAddr;
use std::io;
use std::net::TcpListener as StdTcpListener;
use std::time::{Duration, Instant};

use bytes::Bytes;

use http;
use http::{Request, Response};
use futures::{future, Future, Stream};

use tokio;
use tokio::runtime::Runtime;
use tokio::reactor::Handle;
use tokio::timer::Delay;

use tokio_tcp::TcpListener;

use hyper;
use hyper::Body;
use hyper::client::{Client, HttpConnector};
use hyper::server::conn::Http;
use hyper::service::{service_fn, service_fn_ok};

use tao_log::{debug, debugv, warn};

use body_image::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};
use crate::{AsyncBodyImage, FutioError, RequestRecord, RequestRecorder,
            request_dialog, user_agent};
#[cfg(feature = "mmap")] use crate::{AsyncBodySink, Flaw, UniBodyImage};
use crate::logger::test_logger;

#[test]
fn large_concurrent_gets() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut1, url1) = simple_server(174_333);
    rt.spawn(fut1);
    let (fut2, url2) = simple_server(1_393_400);
    rt.spawn(fut2);

    let tune = Tuner::new()
        .set_max_body_ram(64 * 1024)
        .finish();

    let res = rt.block_on(
        get_req::<Body>(&url1, &tune)
            .join(get_req::<Body>(&url2, &tune))
    );
    match res {
        Ok((dl0, dl1)) => {
            assert_eq!(dl0.res_body().len(),   174_333);
            assert_eq!(dl1.res_body().len(), 1_393_400);
            assert!(!dl0.res_body().is_ram());
            assert!(!dl1.res_body().is_ram());
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn post_echo_body() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image(445);
    match rt.block_on(post_body_req::<Body>(&url, body, &tune)) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn post_echo_async_body() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image(445);
    match rt.block_on(post_body_req::<AsyncBodyImage>(&url, body, &tune)) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_async_body_mmap_copy() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let mut body = fs_body_image(445);
    body.mem_map().unwrap();
    match rt.block_on(post_body_req::<AsyncBodyImage>(&url, body, &tune)) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_uni_body() {
    run_post_echo_uni_body(false);
}

#[test]
#[cfg(feature = "mmap")]
fn post_echo_uni_body_mmap() {
    run_post_echo_uni_body(true);
}

#[cfg(feature = "mmap")]
fn run_post_echo_uni_body(mmap: bool) {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server_uni(mmap);
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(2048)
        .finish();
    let body = fs_body_image(194_767);
    match rt.block_on(post_body_req::<UniBodyImage>(&url, body, &tune)) {
        Ok(dialog) => {
            debugv!(&dialog);
            assert_eq!(dialog.res_body().len(), 194_767);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn timeout_before_response() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = delayed_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_res_timeout(Duration::from_millis(10))
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    match rt.block_on(get_req::<AsyncBodyImage>(&url, &tune)) {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::ResponseTimeout(_) => debug!("expected: {}", e),
            _ => panic!("not response timeout {:?}", e),
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn timeout_during_streaming() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = delayed_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .unset_res_timeout() // workaround, see *_race version of test below
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    match rt.block_on(get_req::<AsyncBodyImage>(&url, &tune)) {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::BodyTimeout(_) => debug!("expected: {}", e),
            _ => panic!("not a body timeout {:?}", e),
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
#[cfg(feature = "may_fail")]
fn timeout_during_streaming_race() {
    assert!(test_logger());

    let mut rt = new_limited_runtime();
    let (fut, url) = delayed_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        // Correct test assertion, but this may fail on CI due to timing
        // issues
        .set_res_timeout(Duration::from_millis(590))
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    match rt.block_on(get_req::<AsyncBodyImage>(&url, &tune)) {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => match e {
            FutioError::BodyTimeout(_) => {},
            _ => panic!("not a body timeout {:?}", e),
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

macro_rules! one_service {
    ($s:ident) => {{
        let (listener, addr) = local_bind().unwrap();
        let fut = listener.incoming()
            .into_future()
            .map_err(|_| -> hyper::Error { unreachable!() })
            .and_then(move |(item, _incoming)| {
                let socket = item.unwrap();
                socket.set_nodelay(true).unwrap();
                Http::new().serve_connection(socket, $s)
            })
            .map_err(|e| warn!("On serve connection: {}", e));
        (fut, format!("http://{}", &addr))
    }}
}

/// The most simple body echo'ing server, using hyper body types.
fn echo_server() -> (impl Future<Item=(), Error=()>, String) {
    let svc = service_fn_ok(move |req: Request<Body>| {
        debugv!("echo server", Response::new(req.into_body()))
    });
    one_service!(svc)
}

/// A body echo'ing server, which buffers complete requests (potentially to
/// disk) using AsyncBodySink and responds with them using a UniBodyImage
#[cfg(feature = "mmap")]
fn echo_server_uni(mmap: bool) -> (impl Future<Item=(), Error=()>, String) {
    let svc = service_fn(move |req: Request<Body>| {
        let tune = Tuner::new()
            .set_buffer_size_fs(2734)
            .set_max_body_ram(15_000)
            .finish();
        let asink = AsyncBodySink::new(
            BodySink::with_ram_buffers(4),
            tune
        );
        req.into_body()
            .from_err::<Flaw>()
            .forward(asink)
            .and_then(move |(_strm, asink)| {
                let tune = Tuner::new().set_buffer_size_fs(4972).finish();
                let mut bi = asink.into_inner().prepare()?;
                if mmap { bi.mem_map()?; }
                Ok(Response::builder()
                   .status(200)
                   .body(debugv!("echo server", UniBodyImage::new(bi, &tune)))?)
            })
    });
    one_service!(svc)
}

/// Server always returning a 1 MiB response body, after delaying before the
/// initial response, and before completing the body. For testing timeouts.
fn delayed_server() -> (impl Future<Item=(), Error=()>, String) {
    let svc = service_fn(move |_req: Request<Body>| {
        let bi = ram_body_image(0x8000, 32);
        let tune = Tunables::default();
        let now = Instant::now();
        let delay1 = tokio::timer::Delay::new(now + Duration::from_millis(100))
            .map_err(|e| -> http::Error { unreachable!(e) });
        let delay2 = Delay::new(now + Duration::from_millis(900))
            .map_err(|e| -> io::Error { unreachable!(e) });
        delay1.and_then(move |()| {
            future::result(Response::builder().status(200).body(
                hyper::Body::wrap_stream(
                    debugv!("delayed", AsyncBodyImage::new(bi, &tune)).select(
                        delay2
                            .map(|_| Bytes::new())
                            .into_stream()
                    )
                )
            ))
        })
    });
    one_service!(svc)
}

fn simple_server(size: usize) -> (impl Future<Item=(), Error=()>, String) {
    let svc = service_fn(move |_req| {
        let bi = fs_body_image(size);
        let tune = Tunables::default();
        Response::builder()
            .status(200)
            .body(debugv!("simple server", AsyncBodyImage::new(bi, &tune)))
    });
    one_service!(svc)
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
    -> impl Future<Item=Dialog, Error=FutioError> + Send
    where T: hyper::body::Payload + Send,
          http::request::Builder: RequestRecorder<T>
{
    let req: RequestRecord<T> = http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header(http::header::USER_AGENT, &user_agent()[..])
        .record()
        .unwrap();
    let connector = HttpConnector::new(1 /*DNS threads*/);
    let client: Client<_, T> = Client::builder().build(connector);
    request_dialog(&client, req, &tune)
}

fn post_body_req<T>(url: &str, body: BodyImage, tune: &Tunables)
    -> impl Future<Item=Dialog, Error=FutioError> + Send
    where T: hyper::body::Payload + Send,
          http::request::Builder: RequestRecorder<T>
{
    let req: RequestRecord<T> = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .record_body_image(body, &tune)
        .unwrap();
    let connector = HttpConnector::new(1 /*DNS threads*/);
    let client: Client<_, T> = Client::builder().build(connector);
    request_dialog(&client, req, &tune)
}

fn new_limited_runtime() -> Runtime {
    tokio::runtime::Builder::new()
        .name_prefix("tpool-")
        .core_threads(2)
        .blocking_threads(2)
        .build()
        .expect("runtime build")
}
