use crate::logger::LOG_SETUP;
use crate::{BodySink, BodyImage, Tunables, Tuner};

use crate::futio::*;

use crate::futio::tokio::runtime::current_thread::Runtime as CtRuntime;
use crate::futio::tokio::runtime::Runtime as DefaultRuntime;

#[test]
fn forward_to_sink_empty() {
    assert!(*LOG_SETUP);
    let tune = Tunables::default();
    let body = UniBodyImage::new(BodyImage::empty(), &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(0), tune.clone());
    let task = body
        .from_err::<Flare>()
        .forward(asink);
    let mut rt = CtRuntime::new().unwrap();
    let res = rt.block_on(task);
    if let Ok((_strm, sink)) = res {
        let out_body = sink.into_inner().prepare().unwrap();
        assert!(out_body.is_ram());
        assert!(out_body.is_empty());
    } else {
        panic!("Failed with {:?}", res);
    }
}

#[test]
fn forward_to_sink_small() {
    assert!(*LOG_SETUP);
    let tune = Tunables::default();
    let body = UniBodyImage::new(BodyImage::from_slice("body"), &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(1), tune.clone());
    let task = body
        .from_err::<Flare>()
        .forward(asink);
    let mut rt = CtRuntime::new().unwrap();
    let res = rt.block_on(task);
    if let Ok((_strm, sink)) = res {
        let out_body = sink.into_inner().prepare().unwrap();
        assert!(out_body.is_ram());
        assert_eq!(out_body.len(), 4);
    } else {
        panic!("Failed with {:?}", res);
    }
}

#[test]
fn forward_to_sink_fs() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new().set_buffer_size_fs(173).finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let abody = UniBodyImage::new(in_body, &tune);
    let asink = UniBodySink::new(
        BodySink::with_fs(tune.temp_dir()).unwrap(),
        tune.clone()
    );
    let task = abody
        .from_err::<Flare>()
        .forward(asink);
    let mut rt = DefaultRuntime::new().unwrap();
    let res = rt.block_on(task);
    if let Ok((_strm, sink)) = res {
        let out_body = sink.into_inner().prepare().unwrap();
        assert!(!out_body.is_ram());
        assert_eq!(out_body.len(), 24_000);
    } else {
        panic!("Failed with {:?}", res);
    }
}

#[test]
fn forward_to_sink_fs_back() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .set_buffer_size_fs(173)
        .set_max_body_ram(15_000)
        .finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let abody = UniBodyImage::new(in_body, &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(4), tune.clone());
    let task = abody
        .from_err::<Flare>()
        .forward(asink);
    let mut rt = DefaultRuntime::new().unwrap();
    let res = rt.block_on(task);
    if let Ok((_strm, sink)) = res {
        let out_body = sink.into_inner().prepare().unwrap();
        assert!(!out_body.is_ram());
        assert_eq!(out_body.len(), 24_000);
    } else {
        panic!("Failed with {:?}", res);
    }
}

#[test]
fn forward_to_sink_fs_map() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .set_buffer_size_fs(173)
        .set_max_body_ram(15_000)
        .finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let mut in_body = in_body.prepare().unwrap();
    assert!(!in_body.is_ram());
    in_body.mem_map().unwrap();
    let abody = UniBodyImage::new(in_body, &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(4), tune.clone());
    let task = abody
        .from_err::<Flare>()
        .forward(asink);
    let mut rt = DefaultRuntime::new().unwrap();
    let res = rt.block_on(task);
    if let Ok((_strm, sink)) = res {
        let out_body = sink.into_inner().prepare().unwrap();
        assert!(!out_body.is_ram());
        assert_eq!(out_body.len(), 24_000);
    } else {
        panic!("Failed with {:?}", res);
    }
}
