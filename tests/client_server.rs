extern crate http;
extern crate hyper;
extern crate fern;
extern crate futures;
#[macro_use] extern crate lazy_static;
extern crate log;

extern crate tokio;
extern crate body_image;

use std::net::SocketAddr;
use std::io;
use std::net::TcpListener as StdTcpListener;

use futures::{Future, Stream};
use tokio::net::{TcpListener};
use tokio::runtime::Runtime;
use tokio::reactor::Handle;

use http::{Request, Response};

use hyper::Body;
use hyper::client::{Client, HttpConnector};
use hyper::server::conn::Http;
use hyper::service::service_fn_ok;

use body_image::{BodySink, Recorded, Tuner};
use body_image::client::{AsyncBodyImage, RequestRecord, RequestRecordable,
                         request_dialog};

fn local_bind() -> Result<(TcpListener, SocketAddr), io::Error> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let std_listener = StdTcpListener::bind(addr).unwrap();
    let listener = TcpListener::from_std(std_listener, &Handle::default())?;
    let local_addr = listener.local_addr()?;
    Ok((listener, local_addr))
}

// Use lazy static to ensure we only setup logging once (by first test and
// thread)
lazy_static! {
    pub static ref LOG_SETUP: bool = setup_logger();
}

#[test]
fn echo_service() {
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

fn setup_logger() -> bool {
    let level = if let Ok(l) = std::env::var("TEST_LOG") {
        l.parse().unwrap()
    } else {
        0
    };
    if level == 0 { return true; }

    let mut disp = fern::Dispatch::new()
        .format(|out, message, record| {
            let t = std::thread::current();
            out.finish(format_args!(
                "{} {} {}: {}",
                record.level(),
                record.target(),
                t.name().map(str::to_owned)
                    .unwrap_or_else(|| format!("{:?}", t.id())),
                message
            ))
        });
    disp = if level == 1 {
        disp.level(log::LevelFilter::Info)
    } else {
        disp.level(log::LevelFilter::Debug)
    };

    if level < 2 {
        // These are only for record/client deps, but are harmless if not
        // loaded.
        disp = disp
            .level_for("hyper::proto",  log::LevelFilter::Info)
            .level_for("tokio_core",    log::LevelFilter::Info)
            .level_for("tokio_reactor", log::LevelFilter::Info);
    }
    disp.chain(std::io::stderr())
        .apply().expect("setup logger");

    true
}
