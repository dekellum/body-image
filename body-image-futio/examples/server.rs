//! This is an example out of convenience rather then being particularly
//! instructive! Its an HTTP server extracted from benches/client.rs for more
//! reliable, out of process benchmarking.

#![warn(rust_2018_idioms)]

use bytes::Bytes;

use futures_util::future;

use http::Response;
use hyper::service::{make_service_fn, service_fn};

use rand::seq::SliceRandom;
use tokio::spawn;
use tokio::runtime::Runtime;

use body_image::{BodyError, BodySink, BodyImage};
use body_image_futio::*;

fn main() {
    let rt = th_runtime();
    let _guard = rt.enter();

    let (url, srv_jh) = {
        let sink = BodySink::with_ram_buffers(1024);
        let body = sink_data(sink).unwrap();
        body_server(body, FutioTunables::default())
    };

    eprintln!("Spawned 2-threaded, in-ram static content server!");
    eprintln!("export BENCH_SERVER_URL={}", url);

    rt.block_on(async {
        srv_jh .await
    }).unwrap().unwrap();
}

fn body_server(body: BodyImage, tune: FutioTunables)
    -> (String,
        tokio::task::JoinHandle<Result<(), hyper::error::Error>>)
{
    let server = hyper::Server::bind(&([127, 0, 0, 1], 9087).into())
        .serve(make_service_fn(move |_| {
            let body = body.clone();
            let tune = tune.clone();
            future::ok::<_, FutioError>(service_fn(move |_req| {
                future::ok::<_, FutioError>(
                    Response::builder()
                        .status(200)
                        .body(AsyncBodyImage::<Bytes>::new(
                            body.clone(), tune.clone()
                        ))
                        .expect("response")
                )
            }))
        }));
    let local_addr = format!("http://{}", server.local_addr()).to_owned();
    let jh = spawn(server);
    (local_addr, jh)
}

/// Return a new body prepared for read, after writing 8MiB of data to the
/// given sink (of any state). All possible u8 values are randomly
/// located within this body.
fn sink_data(mut body: BodySink) -> Result<BodyImage, BodyError> {
    let reps = 1024;
    let mut vals: Vec<u8> = (0..reps).map(|v| (v % 256) as u8).collect();
    vals.shuffle(&mut rand::thread_rng());
    assert!(vals.contains(&255));
    for i in vals {
        body.write_all(vec![i; 0x2000])?;
    }
    let body = body.prepare()?;
    Ok(body)
}

fn th_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_threads(2)
        .enable_io()
        .enable_time()
        .build()
        .expect("threaded runtime build")
}
