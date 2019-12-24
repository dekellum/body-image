use std::future::Future;

use blocking_permit::{DispatchPool, Semaphore, Semaphorish};
use bytes::Bytes;
use futures_core::TryStream;
use futures_util::stream::{FuturesUnordered, StreamExt, TryStreamExt};
use futures_sink::Sink;
use lazy_static::lazy_static;

use body_image::{BodySink, BodyImage, Tuner};

use crate::{
    AsyncBodyImage, AsyncBodySink,
    BlockingPolicy,
    DispatchBodyImage, DispatchBodySink,
    PermitBodyImage, PermitBodySink,
    FutioError, FutioTuner,
    SinkWrapper, StreamWrapper,
};
use crate::logger::test_logger;

#[cfg(feature = "mmap")] use crate::UniBodyBuf;

lazy_static! {
    static ref BLOCKING_TEST_SET: Semaphore = Semaphore::default_new(3);
}

fn register_dispatch() {
    let pool = DispatchPool::builder().pool_size(2).create();
    blocking_permit::register_dispatch_pool(pool);
}

fn deregister_dispatch() {
    blocking_permit::deregister_dispatch_pool();
}

fn th_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new()
        .core_threads(2)
        .max_threads(2+3)
        .threaded_scheduler()
        .build()
        .expect("threaded runtime build")
}

fn local_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new()
        .core_threads(1)
        .max_threads(1+3)
        .basic_scheduler()
        .build()
        .expect("local runtime build")
}

fn empty_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = FutioTuner::new()
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .finish();
    let body = St::new(BodyImage::empty(), tune.clone());

    async move {
        let mut asink = Sk::new(
            BodySink::with_ram_buffers(0),
            tune
        );

        body.err_into::<FutioError>()
            .map_ok(B::from)
            .forward(&mut asink)
            .await?;

        let bsink = asink.into_inner();
        assert!(bsink.is_ram());
        assert!(bsink.is_empty());
        Ok(())
    }
}

#[test]
fn transfer_empty_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = local_runtime();
    let task = empty_task::<
            DispatchBodyImage<Bytes>,
            DispatchBodySink<Bytes>, _>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_empty_th() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = empty_task::<AsyncBodyImage<Bytes>, AsyncBodySink<Bytes>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

fn small_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = FutioTuner::new()
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .finish();
    let body = St::new(BodyImage::from_slice("body"), tune.clone());

    async move {
        let mut asink = Sk::new(
            BodySink::with_ram_buffers(1),
            tune
        );

        body.err_into::<FutioError>()
            .map_ok(B::from)
            .forward(&mut asink)
            .await?;

        let bsink = asink.into_inner();
        assert!(bsink.is_ram());
        assert_eq!(bsink.len(), 4);
        Ok(())
    }
}

#[test]
fn transfer_small_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = local_runtime();
    let task = small_task::<
            DispatchBodyImage<Bytes>,
            DispatchBodySink<Bytes>, _>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_small_th() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = small_task::<AsyncBodyImage<Bytes>, AsyncBodySink<Bytes>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

fn fs_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = FutioTuner::new()
        .set_image(Tuner::new().set_buffer_size_fs(173).finish())
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .finish();
    let mut in_body = BodySink::with_fs(tune.image().temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let body = St::new(in_body, tune.clone());

    async move {
        let mut asink = Sk::new(
            BodySink::with_fs(tune.image().temp_dir()).unwrap(),
            tune
        );

        body.err_into::<FutioError>()
            .map_ok(B::from)
            .forward(&mut asink)
            .await?;

        let bsink = asink.into_inner();
        assert!(!bsink.is_ram());
        assert_eq!(bsink.len(), 24_000);
        Ok(())
    }
}

#[test]
fn transfer_fs_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = local_runtime();
    let task = fs_task::<
            DispatchBodyImage<Bytes>,
            DispatchBodySink<Bytes>, _>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_fs_th() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = fs_task::<AsyncBodyImage<Bytes>, AsyncBodySink<Bytes>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

#[test]
fn transfer_fs_th_permit() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = fs_task::<PermitBodyImage<Bytes>, PermitBodySink<Bytes>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

fn fs_back_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_buffer_size_fs(173)
                .set_max_body_ram(15_000)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .finish();
    let mut in_body = BodySink::with_fs(tune.image().temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let body = St::new(in_body, tune.clone());

    async move {
        let mut asink = Sk::new(
            BodySink::with_ram_buffers(4),
            tune
        );

        body.err_into::<FutioError>()
            .map_ok(B::from)
            .forward(&mut asink)
            .await?;

        let bsink = asink.into_inner();
        assert!(!bsink.is_ram());
        assert_eq!(bsink.len(), 24_000);
        Ok(())
    }
}

#[test]
fn transfer_fs_back_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = local_runtime();
    let task = fs_back_task::<
            DispatchBodyImage<Bytes>,
            DispatchBodySink<Bytes>, _>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_fs_back_th() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = fs_back_task::<
            AsyncBodyImage<Bytes>,
            AsyncBodySink<Bytes>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

#[test]
fn transfer_fs_back_th_permit() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = fs_back_task::<
            PermitBodyImage<Bytes>,
            PermitBodySink<Bytes>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

#[test]
fn transfer_fs_back_th_multi() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let futures: FuturesUnordered<_> = (0..20).map(|_| {
        rt.spawn(fs_back_task::<
                PermitBodyImage<Bytes>,
                PermitBodySink<Bytes>, _>())
    }).collect();
    let join = rt.spawn(async {
        let c = futures.collect::<Vec<_>>() .await;
        assert_eq!(20, c.iter().filter(|r| r.is_ok()).count());
    });
    rt.block_on(join).unwrap();
}

#[cfg(feature = "mmap")]
fn fs_map_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = FutioTuner::new()
        .set_image(
            Tuner::new()
                .set_buffer_size_fs(173)
                .set_max_body_ram(15_000)
                .finish()
        )
        .set_blocking_policy(BlockingPolicy::Permit(&BLOCKING_TEST_SET))
        .finish();
    let mut in_body = BodySink::with_fs(tune.image().temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let mut in_body = in_body.prepare().unwrap();
    in_body.mem_map().unwrap();
    let body = St::new(in_body, tune.clone());

    async move {
        let mut asink = Sk::new(
            BodySink::with_ram_buffers(4),
            tune
        );

        body.err_into::<FutioError>()
            .map_ok(B::from)
            .forward(&mut asink)
            .await?;

        let bsink = asink.into_inner();
        assert!(!bsink.is_ram());
        assert_eq!(bsink.len(), 24_000);
        Ok(())
    }
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_fs_map_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = local_runtime();
    let task = fs_map_task::<
            DispatchBodyImage<UniBodyBuf>,
            DispatchBodySink<UniBodyBuf>, _>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_fs_map_th() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = fs_map_task::<
            AsyncBodyImage<UniBodyBuf>,
            AsyncBodySink<UniBodyBuf>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_fs_map_th_permit() {
    assert!(test_logger());
    let mut rt = th_runtime();
    let task = fs_map_task::<
            PermitBodyImage<UniBodyBuf>,
            PermitBodySink<UniBodyBuf>, _>();
    rt.block_on(rt.spawn(task)).unwrap().unwrap();
}
