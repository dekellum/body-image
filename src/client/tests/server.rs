use std::net::SocketAddr;
use std::io;
use std::net::TcpListener as StdTcpListener;

use ::http;
use ::http::{Request, Response};
use ::logger::LOG_SETUP;
use failure::Error as Flare;

use client::futures::{Future, Stream};

use client::tokio;
use client::tokio::net::{TcpListener};
use client::tokio::runtime::Runtime;
use client::tokio::reactor::Handle;

use client::hyper;
use client::hyper::Body;
use client::hyper::client::{Client, HttpConnector};
use client::hyper::server::conn::Http;
use client::hyper::service::service_fn_ok;

use ::{BodyImage, BodySink, Dialog, Recorded, Tunables, Tuner};
use client::{ AsyncBodyImage,
              RequestRecord, RequestRecordableImage, request_dialog};

#[cfg(feature = "mmap")]
use client::AsyncMemMapBody;

#[test]
fn post_echo_async_body() {
    assert!(*LOG_SETUP);

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let body = fs_body_image();
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
fn post_echo_async_mmap_body() {
    assert!(*LOG_SETUP);

    let mut rt = new_limited_runtime();
    let (fut, url) = echo_server();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let mut body = fs_body_image();
    body.mem_map().unwrap();
    match rt.block_on(post_body_req::<AsyncMemMapBody>(&url, body, &tune)) {
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

fn echo_server() -> (impl Future<Item=(), Error=()>, String) {
    let (listener, addr) = local_bind().unwrap();
    let svc = service_fn_ok(move |req: Request<Body>| {
        Response::builder()
            .status(200)
            .body(req.into_body())
            .unwrap()
    });

    let fut = listener.incoming()
        .into_future()
        .map_err(|_| -> hyper::Error { unreachable!() })
        .and_then(move |(item, _incoming)| {
            let socket = item.unwrap();
            Http::new().serve_connection(socket, svc)
        })
        .map_err(|_| ());

    (fut, format!("http://{}", &addr))
}

fn local_bind() -> Result<(TcpListener, SocketAddr), io::Error> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let std_listener = StdTcpListener::bind(addr).unwrap();
    let listener = TcpListener::from_std(std_listener, &Handle::default())?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
}

fn fs_body_image() -> BodyImage {
    let tune = Tunables::default();
    let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
    body.write_all(
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
         sed do eiusmod tempor incididunt ut labore et dolore magna \
         aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
         ullamco laboris nisi ut aliquip ex ea commodo \
         consequat. Duis aute irure dolor in reprehenderit in \
         voluptate velit esse cillum dolore eu fugiat nulla \
         pariatur. Excepteur sint occaecat cupidatat non proident, \
         sunt in culpa qui officia deserunt mollit anim id est \
         laborum."
    ).unwrap();
    body.prepare().unwrap()
}

fn post_body_req<T>(url: &str, body: BodyImage, tune: &Tunables)
    -> impl Future<Item=Dialog, Error=Flare> + Send
    where T: hyper::body::Payload + Send,
          http::request::Builder: RequestRecordableImage<T>
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
    let mut pool = tokio::executor::thread_pool::Builder::new();
    pool.name_prefix("tpool-")
        .pool_size(2)
        .max_blocking(2);
    tokio::runtime::Builder::new()
        .threadpool_builder(pool)
        .build()
        .expect("runtime build")
}
