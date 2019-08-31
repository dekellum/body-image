#![warn(rust_2018_idioms)]

#![feature(test)]
extern crate test; // Still required, see rust-lang/rust#55133

use std::cmp;
use std::fs;
use std::path::{Path, PathBuf};

use blocking_permit::DispatchPool;
use futures::future;
use futures::stream::{Stream, StreamExt};
use rand::seq::SliceRandom;
use test::Bencher;
use tokio::runtime::{Runtime as ThRuntime, Builder as ThBuilder};
use tokio::runtime::current_thread::Runtime as CtRuntime;
use lazy_static::lazy_static;

use body_image::{BodyError, BodySink, BodyImage, Tunables, Tuner};
use body_image_futio::*;

// `AsyncBodyImage` in `Ram`, pre-gathered (single, contiguous buffer) and
// used for each iteration.
#[bench]
fn stream_01_ram_pregather(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let mut body = sink_data(sink).unwrap();
    body.gather();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `Ram`, scattered state
#[bench]
fn stream_02_ram(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

#[bench]
// `UniBodyImage` in `Ram`, scattered state
fn stream_03_ram_uni(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `Ram`, gathered (single, contiguous buffer) in each
// iteration.
#[bench]
fn stream_04_ram_gather(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let mut body = body.clone();
        body.gather();
        let stream = AsyncBodyImage::new(body, &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `FsRead`, default buffer size (64K), threaded runtime
#[bench]
fn stream_10_fsread(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), threaded runtime.
#[bench]
fn stream_20_fsread_uni(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), threaded runtime,
// dispatch pool.
#[bench]
fn stream_21_fsread_uni_dispatch(b: &mut Bencher) {
    lazy_static! {
        static ref POOL: DispatchPool =
            DispatchPool::builder().pool_size(2).create();
    }
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThBuilder::new()
        .core_threads(2)
        .blocking_threads(1000)
        .after_start(move || {
            DispatchPool::register_thread_local(POOL.clone());
        })
        .build()
        .expect("ThRuntime build");
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), threaded runtime,
// dispatch queue length 0.
#[bench]
fn stream_22_fsread_uni_dispatch_ql0(b: &mut Bencher) {
    lazy_static! {
        static ref POOL: DispatchPool =
            DispatchPool::builder().pool_size(2).queue_length(0).create();
    }
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThBuilder::new()
        .core_threads(2)
        .blocking_threads(1000)
        .after_start(move || {
            DispatchPool::register_thread_local(POOL.clone());
        })
        .build()
        .expect("ThRuntime build");
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), threaded runtime,
// direct run (no dispatch threads).
#[bench]
fn stream_22_fsread_uni_dispatch_direct(b: &mut Bencher) {
    lazy_static! {
        static ref POOL: DispatchPool =
            DispatchPool::builder().pool_size(0).queue_length(0).create();
    }
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThBuilder::new()
        .core_threads(2)
        .blocking_threads(1000)
        .after_start(move || {
            DispatchPool::register_thread_local(POOL.clone());
        })
        .build()
        .expect("ThRuntime build");
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), current thread
// runtime, dispatch pool
#[bench]
fn stream_23_fsread_uni_dispatch_ct(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(2)
        .create();
    DispatchPool::register_thread_local(pool);

    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = CtRuntime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), current thread
// runtime, dispatch queue length 0.
#[bench]
fn stream_24_fsread_uni_dispatch_ct_ql0(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(2)
        .queue_length(0)
        .create();
    DispatchPool::register_thread_local(pool);

    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = CtRuntime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
}

// `UniBodyImage` in `FsRead`, default buffer size (64K), current thread
// runtime, direct run (no dispatch threads).
#[bench]
fn stream_25_fsread_uni_dispatch_ct_direct(b: &mut Bencher) {
    let pool = DispatchPool::builder()
        .pool_size(0)
        .queue_length(0)
        .create();
    DispatchPool::register_thread_local(pool);

    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = CtRuntime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
}

// `AsyncBodyImage` in `FsRead`, 8KiB buffer size
#[bench]
fn stream_30_fsread_8k(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(8 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `FsRead`, 128KiB buffer size
#[bench]
fn stream_31_fsread_128k(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(128 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `FsRead`, 1MiB buffer size
#[bench]
fn stream_32_fsread_1m(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(1024 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `FsRead`, 4MiB buffer size
#[bench]
fn stream_33_fsread_4m(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(4 * 1024 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

#[bench]
// `UniBodyImage` in `MemMap`, mmap once ahead-of-time, zero-copy
fn stream_40_mmap_uni_pre(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let mut body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    body.mem_map().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `UniBodyImage` in `MemMap`, new mmap on each iteration, zero-copy
#[bench]
fn stream_41_mmap_uni(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = UniBodyImage::new(body, &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

// `AsyncBodyImage` in `MemMap`, new mmap on each iteration, and with costly
// copy to `Bytes`.
#[bench]
fn stream_42_mmap_copy(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = ThRuntime::new().unwrap();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = AsyncBodyImage::new(body, &tune);
        summarize_stream(stream, &mut rt);
    });
    rt.shutdown_on_idle();
}

fn summarize_stream<S, T, E, R>(stream: S, rt: &mut R)
    where S: Stream<Item = Result<T, E>> + StreamExt + Send + 'static,
          T: AsRef<[u8]>,
          E: std::fmt::Debug,
          R: RuntimeExt
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
    let (mlast, len) = rt.block_on_pool(task);
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
