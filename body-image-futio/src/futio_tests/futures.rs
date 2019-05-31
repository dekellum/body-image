use futures::stream::Stream;

#[cfg(feature = "futures03")] use {
    futures03::{
        future::{ready as ready_03, TryFutureExt},
        stream::{StreamExt, TryStreamExt},
    },
};

use tokio::runtime::current_thread::Runtime as CtRuntime;
use tokio::runtime::Runtime as DefaultRuntime;

use body_image::{BodySink, BodyImage, Tunables, Tuner};

use crate::{FutioError, UniBodyImage, UniBodySink};
use crate::logger::test_logger;

#[test]
fn forward_to_sink_empty() {
    assert!(test_logger());
    let tune = Tunables::default();
    let body = UniBodyImage::new(BodyImage::empty(), &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(0), tune.clone());
    let task = body
        .from_err::<FutioError>()
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

#[cfg(feature = "futures03")]
#[test]
fn forward_03_to_sink_empty() {
    assert!(test_logger());
    let tune = Tunables::default();
    let body = UniBodyImage::new(BodyImage::empty(), &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(0), tune.clone());
    let task = body
        .err_into::<FutioError>() // 0.3 specific
        .forward(asink);

    let mut rt = CtRuntime::new().unwrap();
    let res: Result<(), FutioError> = rt.block_on(task.compat());

    // FIXME: With 0.3 and these adaptors, we no longer are returned the sink
    // to inspect for proper execution?

    if let Ok(_v) = res {
        //let bsink = asink03.get_ref().body();
        //assert!(bsink.is_ram());
        //assert!(bsink.is_empty());
    } else {
        panic!("Failed with {:?}", res);
    }
}

#[test]
fn forward_to_sink_small() {
    assert!(test_logger());
    let tune = Tunables::default();
    let body = UniBodyImage::new(BodyImage::from_slice("body"), &tune);
    let asink = UniBodySink::new(BodySink::with_ram_buffers(1), tune.clone());
    let task = body
        .from_err::<FutioError>()
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
    assert!(test_logger());
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
        .from_err::<FutioError>()
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

#[cfg(feature = "futures03")]
#[test]
fn forward_03_to_sink_fs() {
    assert!(test_logger());
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
        .err_into::<FutioError>() // 0.3 specific
        .forward(asink)
        .and_then(|v: ()| { // also just `Ok(())` here. can't inspect
            ready_03(Ok(v))
        });
    let mut rt = DefaultRuntime::new().unwrap();
    let res: Result<(), FutioError> = rt.block_on(task.compat());

    // FIXME: With 0.3 and these adaptors, we no longer are returned the sink
    // to inspect for proper execution?
    res.expect("task success");

    // assert!(!out_body.is_ram());
    // assert_eq!(out_body.len(), 24_000);
}

#[test]
fn forward_to_sink_fs_back() {
    assert!(test_logger());
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
        .from_err::<FutioError>()
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
    assert!(test_logger());
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
        .from_err::<FutioError>()
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
