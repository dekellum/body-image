#![warn(rust_2018_idioms)]

#![feature(test)]
extern crate test; // Still required, see rust-lang/rust#55133

use std::cmp;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use blocking_permit::{
    DispatchPool, Semaphore, Semaphorish,
    deregister_dispatch_pool, register_dispatch_pool,
};
use bytes::Bytes;

use futures_core::stream::Stream;
use futures_util::future;
use futures_util::future::FutureExt;
use futures_util::stream::StreamExt;
use futures_util::stream::FuturesUnordered;

use http::Response;
use hyper::client::{Client, HttpConnector};
use hyper::service::{make_service_fn, service_fn};

use lazy_static::lazy_static;
use rand::seq::SliceRandom;
use test::Bencher;
use tokio::spawn;
use tokio::runtime::Runtime;

use body_image::{BodyError, BodySink, BodyImage, Recorded, Tuner};
use body_image_futio::*;

const CORE_THREADS: usize      =   2;
const EXTRA_THREADS: usize     =   2;
const BATCH: usize             =  16;

lazy_static! {
    static ref BLOCKING_SET: Semaphore = Semaphore::default_new(EXTRA_THREADS);
}

#[bench]
fn client_01_ram(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_max_body_ram(0x2000 * 1025).finish())
        .finish();
    client_run::<AsyncBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[bench]
fn client_02_ram_gather(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_max_body_ram(0x2000 * 1025).finish())
        .finish();
    client_run::<AsyncBodyImage<Bytes>, _, _>(rt, tune, ClientOp::Gather, b);
}

#[bench]
fn client_03_ram_gather_yield(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_max_body_ram(0x2000 * 1025).finish())
        .set_stream_item_size(2 * 1024 * 1024) // huge page size
        .finish();
    client_run::<SplitBodyImage<Bytes>, _, _>(rt, tune, ClientOp::Gather, b);
}

#[cfg(feature = "tangential")]
#[bench]
fn client_03_ram_uni(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_max_body_ram(0x2000 * 1025).finish())
        .finish();
    client_run::<AsyncBodyImage<UniBodyBuf>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[bench]
fn client_20_fs_direct(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Direct)
        .finish();
    client_run::<AsyncBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[bench]
fn client_21_fs_yield(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Direct)
        .finish();
    client_run::<YieldBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[bench]
fn client_22_fs_permit(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS, Some(EXTRA_THREADS), None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    client_run::<PermitBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[cfg(feature = "tangential")]
#[bench]
fn client_23_fs_dispatch1(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(1)
        .queue_length(EXTRA_THREADS)
        .create();
    let rt = mt_runtime(CORE_THREADS, None, Some(pool));
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    client_run::<DispatchBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[bench]
fn client_24_fs_dispatch(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(EXTRA_THREADS)
        .queue_length(EXTRA_THREADS)
        .create();
    let rt = mt_runtime(CORE_THREADS, None, Some(pool));
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    client_run::<DispatchBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[cfg(feature = "tangential")]
#[bench]
fn client_25_fs_dispatch3(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(3)
        .queue_length(EXTRA_THREADS)
        .create();
    let rt = mt_runtime(CORE_THREADS, None, Some(pool));
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    client_run::<DispatchBodyImage<Bytes>, _, _>(rt, tune, ClientOp::AsIs, b);
}

#[cfg(feature = "mmap")]
#[bench]
fn client_30_mmap_direct_copy(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Direct)
        .finish();
    client_run::<AsyncBodyImage<Bytes>, _, _>(rt, tune, ClientOp::Mmap, b);
}

#[cfg(feature = "tangential")]
#[cfg(feature = "mmap")]
#[bench]
fn client_31_mmap_permit_copy(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS, Some(EXTRA_THREADS), None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    client_run::<AsyncBodyImage<Bytes>, _, _>(rt, tune, ClientOp::Mmap, b);
}

#[cfg(feature = "mmap")]
#[bench]
fn client_35_mmap_direct(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Direct)
        .finish();
    client_run::<AsyncBodyImage<UniBodyBuf>, _, _>(rt, tune, ClientOp::Mmap, b);
}

#[cfg(feature = "mmap")]
#[bench]
fn client_36_mmap_yield(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS+EXTRA_THREADS, None, None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Direct)
        .set_stream_item_size(2 * 1024 * 1024) // huge page size
        .finish();
    client_run::<SplitBodyImage<UniBodyBuf>, _, _>(rt, tune, ClientOp::Mmap, b);
}

#[cfg(feature = "mmap")]
#[bench]
fn client_37_mmap_permit(b: &mut Bencher) {
    let rt = mt_runtime(CORE_THREADS, Some(EXTRA_THREADS), None);
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    client_run::<PermitBodyImage<UniBodyBuf>, _, _>(rt, tune, ClientOp::Mmap, b);
}

#[cfg(feature = "mmap")]
#[bench]
fn client_38_mmap_dispatch(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(EXTRA_THREADS)
        .queue_length(EXTRA_THREADS)
        .create();
    let rt = mt_runtime(CORE_THREADS, None, Some(pool));
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_temp_dir(test_path().unwrap())
                .set_max_body_ram(0)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    client_run::<DispatchBodyImage<UniBodyBuf>, _, _>(rt, tune, ClientOp::Mmap, b);
}

#[derive(Copy, Clone)]
enum ClientOp {
    AsIs,
    #[cfg(feature = "mmap")]
    Mmap,
    Gather
}

fn client_run<I, T, E>(
    rt: Runtime,
    tune: FutioTunables,
    op: ClientOp,
    b: &mut Bencher)
    where I: StreamWrapper + Send,
          I: Stream<Item = Result<T, E>> + StreamExt + Send + 'static,
          T: AsRef<[u8]> + 'static,
          E: std::fmt::Debug + 'static
{
    let _guard = rt.enter();
    // Use external server if provided URL in env var, else spawn our own, in
    // process.
    let (url, shutdown_tx, srv_jh) =
        if let Ok(uv) = env::var("BENCH_SERVER_URL")
    {
        (uv, None, None)
    } else {
        let (url, tx, jh) = {
            let sink = BodySink::with_ram_buffers(1024);
            let body = sink_data(sink).unwrap();
            body_server(body, FutioTunables::default())
        };
        (url, Some(tx), Some(jh))
    };

    b.iter(|| {
        let tune = tune.clone();
        let url = url.clone();
        let connector = HttpConnector::new();
        let client = Client::builder().build(connector);
        let job = async move {
            let futures: FuturesUnordered<_> = (0..BATCH).map(|_| {
                let tune2 = tune.clone();
                let op = op;
                // Empty request body, type doesn't matter.
                let req: RequestRecord<AsyncBodyImage<Bytes>> =
                    http::Request::builder()
                    .method(http::Method::GET)
                    .uri(&url)
                    .record()
                    .unwrap();
                let req = request_dialog(&client, req, tune.clone())
                    .then(move |r| {
                        let mut body = r.unwrap().res_body().clone();
                        match op {
                            ClientOp::AsIs => {},
                            #[cfg(feature = "mmap")]
                            ClientOp::Mmap => {
                                body.mem_map().unwrap();
                            }
                            ClientOp::Gather => {
                                body.gather();
                            }
                        }
                        summarize_stream(I::new(body, tune2))
                    });
                spawn(req)
            }).collect();
            let c = futures.collect::<Vec<_>>() .await;
            assert_eq!(BATCH, c.iter().filter(|r| r.is_ok()).count());
        };
        rt.block_on(rt.spawn(job)).unwrap();
    });

    if let Some(tx) = shutdown_tx {
        tx.send(()).unwrap();
    }
    if let Some(jh) = srv_jh {
        rt.block_on(async {
            jh .await
        }).unwrap().unwrap();
    }
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
            rx .await .ok();
        });
    let jh = spawn(server);
    (local_addr, tx, jh)
}

async fn summarize_stream<S, T, E>(stream: S)
    where S: Stream<Item = Result<T, E>> + StreamExt + Send + 'static,
          T: AsRef<[u8]>,
          E: std::fmt::Debug
{
    let (mlast, len) = stream.fold((0u8, 0), |(mut ml, len), item| {
        let item = item.unwrap();
        let item = item.as_ref();
        let mut i = 0;
        let e = item.len();
        while i < e {
            ml = cmp::max(ml, item[i]);
            i += 1973; // prime < (0x1000/2)
        }
        future::ready((ml, len + item.len()))
    }) .await;
    assert_eq!(mlast, 255);
    assert_eq!(len, 0x2000 * 1024);
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

fn test_path() -> Result<PathBuf, Flaw> {
    let target = env!("CARGO_MANIFEST_DIR");
    let path = format!("{}/../target/testmp", target);
    let tpath = Path::new(&path);
    fs::create_dir_all(tpath)?;
    Ok(tpath.to_path_buf())
}

fn mt_runtime(
    core: usize,
    blocking: Option<usize>,
    dispatch: Option<DispatchPool>)
    -> Runtime
{
    struct AbortOnPanic;

    impl Drop for AbortOnPanic {
        fn drop(&mut self) {
            std::process::abort();
        }
    }

    let mut bldr = tokio::runtime::Builder::new_multi_thread();
    bldr.worker_threads(core);

    let extra_threads = match blocking {
        Some(c) => c,
        None => 1
    };
    bldr.max_blocking_threads(extra_threads);

    if let Some(pool) = dispatch {
        bldr.on_thread_start(move || {
            register_dispatch_pool(pool.clone());
        });
        bldr.on_thread_stop(|| {
            deregister_dispatch_pool();
        });
    }
    let cntr = AtomicUsize::new(0);
    let mut max = core;
    if let Some(b) = blocking {
        max += b;
    }
    bldr.thread_name_fn(move || {
        let c = cntr.fetch_add(1, Ordering::SeqCst);
        if c >= max {
            let _aborter = AbortOnPanic;
            panic!("spawn_blocking/block_in_place must have been used!");
        } else {
            format!("worker-{}", c)
        }
    });

    bldr.enable_io()
        .enable_time()
        .build()
        .unwrap()
}
