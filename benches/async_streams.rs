#![feature(test)]
extern crate test;
extern crate body_image;
extern crate failure;
extern crate futures;
extern crate tokio;

use std::cmp;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use test::Bencher;

use failure::Error as Flare;

use futures::Stream;

use body_image::{BodySink, BodyImage, Tunables, Tuner};
use body_image::async::*;

// `AsyncBodyImage` in `Ram`, pre-gathered (single, contiguous buffer) and
// used for each iteration.
#[bench]
fn stream_01_ram_pregather(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let mut body = sink_data(sink).unwrap();
    body.gather();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `Ram`, scattered state
#[bench]
fn stream_02_ram(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

#[bench]
// `UniBodyImage` in `Ram`, scattered state
fn stream_03_ram_uni(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `Ram`, gathered (single, contiguous buffer) in each
// iteration.
#[bench]
fn stream_04_ram_gather(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_ram_buffers(1024);
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let mut body = body.clone();
        body.gather();
        let stream = AsyncBodyImage::new(body, &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `FsRead`, default buffer size (64K)
#[bench]
fn stream_10_fsread(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `UniBodyImage` in `FsRead`, default buffer size (64K)
#[bench]
fn stream_11_fsread_uni(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `FsRead`, 8KiB buffer size
#[bench]
fn stream_12_fsread_8k(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(8 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `FsRead`, 128KiB buffer size
#[bench]
fn stream_13_fsread_128k(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(128 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `FsRead`, 1MiB buffer size
#[bench]
fn stream_14_fsread_1m(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(1024 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `FsRead`, 4MiB buffer size
#[bench]
fn stream_15_fsread_4m(b: &mut Bencher) {
    let tune = Tuner::new().set_buffer_size_fs(4 * 1024 * 1024).finish();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let stream = AsyncBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

#[bench]
// `UniBodyImage` in `MemMap`, mmap once ahead-of-time, zero-copy
fn stream_20_mmap_uni_pre(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let mut body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    body.mem_map().unwrap();
    b.iter(|| {
        let stream = UniBodyImage::new(body.clone(), &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `UniBodyImage` in `MemMap`, new mmap on each iteration, zero-copy
#[bench]
fn stream_21_mmap_uni(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = UniBodyImage::new(body, &tune);
        summarize_stream(stream, &mut rt);
    })
}

// `AsyncBodyImage` in `MemMap`, new mmap on each iteration, and with costly
// copy to `Bytes`.
#[bench]
fn stream_22_mmap_copy(b: &mut Bencher) {
    let tune = Tunables::default();
    let sink = BodySink::with_fs(test_path().unwrap()).unwrap();
    let body = sink_data(sink).unwrap();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    b.iter(|| {
        let mut body = body.clone();
        body.mem_map().unwrap();
        let stream = AsyncBodyImage::new(body, &tune);
        summarize_stream(stream, &mut rt);
    })
}

fn summarize_stream<S>(stream: S, rt: &mut tokio::runtime::Runtime)
    where S: Stream<Error=io::Error> + Send + 'static,
          S::Item: AsRef<[u8]>
{
    let task = stream.fold((0u8,0), |(ml, len), item| -> Result<_,io::Error> {
        Ok((
            cmp::max(ml, *item.as_ref().last().unwrap()),
            len + item.as_ref().len()
        ))
    });
    let res = rt.block_on(task);
    if let Ok((mlast, len)) = res {
        assert_eq!(mlast, 255);
        assert_eq!(len, 8192 * 1024);
    } else {
        panic!("Failed {:?}", res);
    }
}

fn sink_data(mut body: BodySink) -> Result<BodyImage, Flare> {
    let reps = 1024;
    for i in 0..reps {
        body.write_all(vec![i as u8; 8192])?;
    }
    let body = body.prepare()?;
    Ok(body)
}

fn test_path() -> Result<PathBuf, Flare> {
    let tpath = Path::new("target/testmp");
    fs::create_dir_all(tpath)?;
    Ok(tpath.to_path_buf())
}