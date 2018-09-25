use std::net::SocketAddr;
use std::io;
use std::net::TcpListener as StdTcpListener;
use std::time::{Duration, Instant};

use ::bytes::Bytes;

use ::http;
use ::http::{Request, Response};
use crate::logger::LOG_SETUP;

use failure::Error as Flare;

use crate::futio::futures::{future, Future, Stream};

use crate::futio::tokio;
use crate::futio::tokio::net::TcpListener;
use crate::futio::tokio::runtime::Runtime;
use crate::futio::tokio::reactor::Handle;
use crate::futio::tokio::timer::Delay;

use crate::futio::hyper;
use crate::futio::hyper::Body;
use crate::futio::hyper::client::{Client, HttpConnector};
use crate::futio::hyper::server::conn::Http;
use crate::futio::hyper::service::{service_fn, service_fn_ok};

use crate::futio::{AsyncBodyImage, RequestRecord, RequestRecorder,
                   request_dialog, user_agent};

#[cfg(feature = "mmap")] use crate::r#async::{AsyncBodySink, UniBodyImage};

use crate::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};

#[test]
fn large_concurrent_gets() {
    assert!(*LOG_SETUP);

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
            assert_eq!(dl0.res_body.len(),   174_333);
            assert_eq!(dl1.res_body.len(), 1_393_400);
            assert!(!dl0.res_body.is_ram());
            assert!(!dl1.res_body.is_ram());
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn post_echo_body() {
    assert!(*LOG_SETUP);

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image(445);
    match rt.block_on(post_body_req::<Body>(&url, body, &tune)) {
        Ok(dl) => {
            println!("{:#?}", dl);
            assert_eq!(dl.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn post_echo_async_body() {
    assert!(*LOG_SETUP);

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image(445);
    match rt.block_on(post_body_req::<AsyncBodyImage>(&url, body, &tune)) {
        Ok(dl) => {
            println!("{:#?}", dl);
            assert_eq!(dl.res_body().len(), 445);
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
    assert!(*LOG_SETUP);

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let mut body = fs_body_image(445);
    body.mem_map().unwrap();
    match rt.block_on(post_body_req::<AsyncBodyImage>(&url, body, &tune)) {
        Ok(dl) => {
            println!("{:#?}", dl);
            assert_eq!(dl.res_body().len(), 445);
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
    assert!(*LOG_SETUP);

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server_uni(mmap);
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(2048)
        .finish();
    let body = fs_body_image(194_767);
    match rt.block_on(post_body_req::<UniBodyImage>(&url, body, &tune)) {
        Ok(dl) => {
            println!("{:#?}", dl);
            assert_eq!(dl.res_body().len(), 194_767);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn timeout_before_response() {
    assert!(*LOG_SETUP);

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
        Err(e) => {
            let em = e.to_string();
            assert!(em.starts_with("timeout"), em);
            assert!(em.contains("initial"), em);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
fn timeout_during_streaming() {
    assert!(*LOG_SETUP);

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
        Err(e) => {
            let em = e.to_string();
            assert!(em.starts_with("timeout"), em);
            assert!(em.contains("streaming"), em);
        }
    }
    rt.shutdown_on_idle().wait().unwrap();
}

#[test]
#[cfg(feature = "may_fail")]
fn timeout_during_streaming_race() {
    assert!(*LOG_SETUP);

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
        Err(e) => {
            let em = e.to_string();
            assert!(em.starts_with("timeout"), em);
            assert!(em.contains("streaming"), em);
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
        Response::new(req.into_body())
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
            .from_err::<Flare>()
            .forward(asink)
            .and_then(move |(_strm, asink)| {
                let tune = Tuner::new().set_buffer_size_fs(4972).finish();
                let mut bi = asink.into_inner().prepare()?;
                if mmap { bi.mem_map()?; }
                Ok(Response::builder()
                   .status(200)
                   .body(UniBodyImage::new(bi, &tune))?)
            })
            .map_err(|e| e.compat())
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
                    AsyncBodyImage::new(bi, &tune).select(
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
    let svc = service_fn( move |_req| {
        let bi = fs_body_image(size);
        let tune = Tunables::default();
        Response::builder()
           .status(200)
           .body(AsyncBodyImage::new(bi, &tune))
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
    -> impl Future<Item=Dialog, Error=Flare> + Send
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
    -> impl Future<Item=Dialog, Error=Flare> + Send
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
