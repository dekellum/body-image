#![warn(rust_2018_idioms)]

#![feature(test)]
extern crate test; // Still required, see rust-lang/rust#55133

use std::cmp;
use std::fs;
use std::path::{Path, PathBuf};

use blocking_permit::{
    DispatchPool, Semaphore, Semaphorish,
    register_dispatch_pool, deregister_dispatch_pool
};
use bytes::Bytes;
use futures_core::stream::Stream;
use futures_util::future;
use futures_util::stream::StreamExt;
use lazy_static::lazy_static;
use rand::seq::SliceRandom;
use test::Bencher;

use body_image::{BodyError, BodySink, BodyImage};

#[cfg(feature = "tangential")]
use body_image::Tuner;

use body_image_futio::*;

lazy_static! {
    static ref BLOCKING_SET: Semaphore = Semaphore::default_new(2);
}

// `AsyncBodyImage` in `Ram`, pre-gathered (single, contiguous buffer) and
// used for each iteration.
#[bench]
fn stream_01_ram_pregather(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let mut body = sink_data(sink).unwrap();
    body.gather();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = AsyncBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `Ram`, scattered state
#[bench]
fn stream_02_ram(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = AsyncBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `Ram`, scattered state
#[bench]
fn stream_03_ram_uni(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = AsyncBodyImage::<UniBodyBuf>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `Ram`, gathered (single, contiguous buffer) in each
// iteration.
#[bench]
fn stream_04_ram_gather(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let mut body = body.clone();
        body.gather();
        let stream = AsyncBodyImage::<Bytes>::new(body, tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, default buffer size (64K), threaded runtime
#[bench]
fn stream_20_fsread_direct(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = AsyncBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, default buffer size (64K), threaded runtime
#[bench]
fn stream_20_fsread_permit(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = PermitBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), threaded runtime,
// dispatch pool.
#[bench]
fn stream_21_fsread_dispatch1(b: &mut Bencher) {
    let tune = FutioTuner::new()
        // not essential, but consistent
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let pool = DispatchPool::builder().pool_size(1).create();
    let mut rt = th_dispatch_runtime(pool);
    b.iter(|| {
        let stream = DispatchBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), threaded runtime,
// dispatch pool.
#[bench]
fn stream_21_fsread_dispatch2(b: &mut Bencher) {
    let tune = FutioTuner::new()
        // not essential, but consistent
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let pool = DispatchPool::builder().pool_size(2).create();
    let mut rt = th_dispatch_runtime(pool);
    b.iter(|| {
        let stream = DispatchBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), threaded runtime,
// dispatch queue length 0.
#[bench]
fn stream_22_fsread_dispatch1_ql0(b: &mut Bencher) {
    let tune = FutioTuner::new()
        // not essential, but consistent
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let pool = DispatchPool::builder().pool_size(1).queue_length(0).create();
    let mut rt = th_dispatch_runtime(pool);
    b.iter(|| {
        let stream = DispatchBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), threaded runtime,
// direct run (no dispatch threads).
#[bench]
fn stream_22_fsread_uni_direct(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = AsyncBodyImage::<UniBodyBuf>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), current thread
// runtime, dispatch pool
#[bench]
fn stream_23_fsread_dispatch1_ct(b: &mut Bencher) {
    let pool = DispatchPool::builder().pool_size(1).create();
    register_dispatch_pool(pool);
    let tune = FutioTuner::new()
        // not essential, but consistent
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = local_runtime();
    b.iter(|| {
        let stream = DispatchBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
    deregister_dispatch_pool();
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), current thread
// runtime, dispatch queue length 0.
#[bench]
fn stream_24_fsread_dispatch1_ct_ql0(b: &mut Bencher) {
    let pool = DispatchPool::builder().queue_length(0).create();
    register_dispatch_pool(pool);
    let tune = FutioTuner::new()
        // not essential, but consistent
        .set_blocking_policy(BlockingPolicy::Dispatch)
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = local_runtime();
    b.iter(|| {
        let stream = DispatchBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
    deregister_dispatch_pool();
}

// `AsyncBodyImage::<UniBodyBuf>` in `FsRead`, default buffer size (64K), current thread
// runtime, direct run (no dispatch threads).
#[bench]
fn stream_25_fsread_direct_ct(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = local_runtime();
    b.iter(|| {
        let stream = AsyncBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, 8KiB buffer size
#[cfg(feature = "tangential")]
#[bench]
fn stream_30_fsread_permit_8k(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(8 * 1024).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = PermitBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, 128KiB buffer size
#[cfg(feature = "tangential")]
#[bench]
fn stream_31_fsread_permit_128k(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(128 * 1024).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = PermitBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, 1MiB buffer size
#[cfg(feature = "tangential")]
#[bench]
fn stream_32_fsread_permit_1m(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(1024 * 1024).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = PermitBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, 4MiB buffer size
#[cfg(feature = "tangential")]
#[bench]
fn stream_33_fsread_permit_4m(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(4 * 1024 * 1024).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let stream = PermitBodyImage::<Bytes>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `MemMap`, mmap once ahead-of-time, zero-copy
#[bench]
#[cfg(feature = "mmap")]
fn stream_40_mmap_pre(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let mut body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    body.mem_map().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::<UniBodyBuf>::new(body.clone(), tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `MemMap`, new mmap on each iteration, zero-copy
#[bench]
#[cfg(feature = "mmap")]
fn stream_41_mmap_direct(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = AsyncBodyImage::<UniBodyBuf>::new(body, tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage::<UniBodyBuf>` in `MemMap`, new mmap on each iteration, zero-copy
#[bench]
#[cfg(feature = "mmap")]
fn stream_41_mmap_permit(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = PermitBodyImage::<UniBodyBuf>::new(body, tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `MemMap`, new mmap on each iteration, and with costly
// copy to `Bytes`.
#[bench]
#[cfg(feature = "mmap")]
fn stream_42_mmap_copy_direct(b: &mut Bencher) {
    let tune = FutioTunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = AsyncBodyImage::<Bytes>::new(body, tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `MemMap`, new mmap on each iteration, and with costly
// copy to `Bytes`.
#[bench]
#[cfg(feature = "mmap")]
fn stream_42_mmap_copy_permit(b: &mut Bencher) {
    let tune = FutioTuner::new()
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_SET))
        .finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = th_runtime();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = PermitBodyImage::<Bytes>::new(body, tune.clone());
        summarize_stream(stream, &mut rt);
    });
}

fn summarize_stream<S, T, E>(stream: S, rt: &mut tokio::runtime::Runtime)
    where S: Stream<Item = Result<T, E>> + StreamExt + Send + 'static,
          T: AsRef<[u8]>,
          E: std::fmt::Debug
{
    let task = stream.fold((0u8, 0), |(mut ml, len), item| {
        let item = item.unwrap();
        let item = item.as_ref();
        let mut i = 0;
        let e = item.len();
        while i < e {
            ml = cmp::max(ml, item[i]);
            i += 1973; // prime < (0x1000/2)
        }
        future::ready((ml, len + item.len()))
    });
    let (mlast, len) = rt.block_on(rt.spawn(task)).unwrap();
    assert_eq!(mlast, 255);
    assert_eq!(len, 8192 * 1024);
}

/// Return a new body prepared for read, after writing 8MiB of data the the
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

fn th_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new()
        .core_threads(2)
        .max_threads(2+2)
        .threaded_scheduler()
        .build()
        .expect("threaded runtime build")
}

fn th_dispatch_runtime(pool: DispatchPool) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new()
        .core_threads(2)
        .max_threads(2)
        .threaded_scheduler()
        .on_thread_start(move || {
            register_dispatch_pool(pool.clone());
        })
        .build()
        .expect("threaded dispatch runtime build")
}

fn local_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new()
        .core_threads(1)
        .max_threads(4)
        .basic_scheduler()
        .build()
        .expect("local runtime build")
}
