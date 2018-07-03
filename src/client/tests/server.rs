use std::net::SocketAddr;
use std::io;
use std::net::TcpListener as StdTcpListener;

use ::http;
use ::http::{Request, Response};
use ::logger::LOG_SETUP;

use client::futures::{Future, Stream};
use client::tokio::net::{TcpListener};
use client::tokio::runtime::Runtime;
use client::tokio::reactor::Handle;

use client::hyper;
use client::hyper::Body;
use client::hyper::client::{Client, HttpConnector};
use client::hyper::server::conn::Http;
use client::hyper::service::service_fn_ok;

use ::{BodySink, Recorded, Tuner};
use client::{AsyncBodyImage, RequestRecord, RequestRecordable,
               request_dialog};

fn local_bind() -> Result<(TcpListener, SocketAddr), io::Error> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let std_listener = StdTcpListener::bind(addr).unwrap();
    let listener = TcpListener::from_std(std_listener, &Handle::default())?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
}


#[test]
fn post_echo_async_body() {
    assert!(*LOG_SETUP);
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

    let mut rt = Runtime::new().unwrap();
    rt.spawn(fut);

    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
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
    let body = body.prepare().unwrap();
    let rq: RequestRecord<AsyncBodyImage> = http::Request::builder()
        .method(http::Method::POST)
        // To avoid 27 chunks
        // .header(http::header::CONTENT_LENGTH, "445")
        .uri(format!("http://{}", &addr))
        .record_body_image(body, &tune)
        .unwrap();

    let connector = HttpConnector::new(1 /*DNS threads*/);
    let client: Client<_, AsyncBodyImage> = Client::builder().build(connector);
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(dl) => {
            println!("{:#?}", dl);
            assert_eq!(dl.res_body().len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }

    drop(client);
    rt.shutdown_on_idle().wait().unwrap();
}
