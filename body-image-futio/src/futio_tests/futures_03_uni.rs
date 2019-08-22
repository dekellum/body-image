use futures::{
    future::FutureExt,
    stream::{StreamExt, TryStreamExt},
};

use tao_log::debug;

use tokio::runtime;
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

    let task = async move {
        let mut asink = UniBodySink::new(
            BodySink::with_ram_buffers(0),
            tune
        );

        body.err_into::<FutioError>()
            .forward(&mut asink)
            .await?;

        let bsink = asink.body();
        assert!(bsink.is_ram());
        assert!(bsink.is_empty());
        Ok(())
    };

    let mut rt = CtRuntime::new().unwrap();
    let res: Result<(), FutioError> = rt.block_on(task);
    res.expect("task success");
}

#[test]
fn forward_to_sink_small() {
    assert!(test_logger());
    let tune = Tunables::default();
    let body = UniBodyImage::new(BodyImage::from_slice("body"), &tune);

    let task = async move {
        let mut asink = UniBodySink::new(
            BodySink::with_ram_buffers(1),
            tune
        );

        body.err_into::<FutioError>()
            .forward(&mut asink)
            .await?;

        let bsink = asink.body();
        assert!(bsink.is_ram());
        assert_eq!(bsink.len(), 4);
        Ok(())
    };

    let mut rt = CtRuntime::new().unwrap();
    let res: Result<(), FutioError> = rt.block_on(task);
    res.expect("task success");
}

#[test]
fn forward_to_sink_fs() {
    assert!(test_logger());

    let tune = Tuner::new().set_buffer_size_fs(173).finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let body = UniBodyImage::new(in_body, &tune);

    let task = async move {
        let mut asink = UniBodySink::new(
            BodySink::with_fs(tune.temp_dir()).unwrap(),
            tune
        );

        body.err_into::<FutioError>()
            .forward(&mut asink)
            .await?;

        let bsink = asink.body();
        assert!(!bsink.is_ram());
        assert_eq!(bsink.len(), 24_000);
        Ok(())
    };

    let rt = DefaultRuntime::new().unwrap();
    rt.spawn(task.map(|_r: Result<(), FutioError>| () ));
    rt.shutdown_on_idle();

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
    let body = UniBodyImage::new(in_body, &tune);

    let task = async move {
        let mut asink = UniBodySink::new(
            BodySink::with_ram_buffers(4),
            tune
        );

        body.err_into::<FutioError>()
            .forward(&mut asink)
            .await?;

        let bsink = asink.body();
        assert!(!bsink.is_ram());
        assert_eq!(bsink.len(), 24_000);
        Ok(())
    };

    let rt = DefaultRuntime::new().unwrap();
    rt.spawn(task.map(|_r: Result<(), FutioError>| () ));
    rt.shutdown_on_idle();
}

#[test]
fn forward_to_sink_fs_back_concurrent() {
    assert!(test_logger());

    let tune = Tuner::new()
        .set_buffer_size_fs(173)
        .set_max_body_ram(15_000)
        .finish();
    let rt = runtime::Builder::new()
        .name_prefix("tpool-")
        .core_threads(3)
        .blocking_threads(1)
        .build()
        .expect("runtime build");

    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();

    for i in 1..20 {
        let tune = tune.clone();
        let in_body = in_body.clone();

        let task = async move {
            let body = UniBodyImage::new(in_body, &tune);

            let mut asink = UniBodySink::new(
                BodySink::with_ram_buffers(4),
                tune
            );

            body.err_into::<FutioError>()
                .forward(&mut asink)
                .await?;

            let bsink = asink.body();
            assert!(!bsink.is_ram());
            assert_eq!(bsink.len(), 24_000);

            debug!("success {}", i);
            Ok(())
        };

        rt.spawn(task.map(|_r: Result<(), FutioError>| () ));
    }
    rt.shutdown_on_idle();
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
    in_body.mem_map().unwrap();
    let body = UniBodyImage::new(in_body, &tune);

    let task = async move {
        let mut asink = UniBodySink::new(
            BodySink::with_ram_buffers(4),
            tune
        );

        body.err_into::<FutioError>()
            .forward(&mut asink)
            .await?;

        let bsink = asink.body();
        assert!(!bsink.is_ram());
        assert_eq!(bsink.len(), 24_000);
        Ok(())
    };

    let rt = DefaultRuntime::new().unwrap();
    rt.spawn(task.map(|_r: Result<(), FutioError>| () ));
    rt.shutdown_on_idle();
}
