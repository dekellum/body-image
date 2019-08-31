use std::future::Future;

use blocking_permit::DispatchPool;
use futures::{
    future::FutureExt,
    sink::Sink,
    stream::{StreamExt, TryStream, TryStreamExt},
};
use tokio::runtime::{Runtime as ThRuntime, Builder as ThBuilder};
use tokio::runtime::current_thread::Runtime as CtRuntime;

use body_image::{BodySink, BodyImage, Tunables, Tuner};

use crate::{
    ensure_min_blocking_set,
    FutioError, AsyncBodyImage, AsyncBodySink,
    RuntimeExt, SinkWrapper, StreamWrapper
};
use crate::logger::test_logger;

#[cfg(feature = "mmap")]
use crate::{UniBodyBuf, UniBodyImage, UniBodySink};

fn register_dispatch() {
    let pool = DispatchPool::builder().pool_size(2).create();
    DispatchPool::register_thread_local(pool);
}

fn deregister_dispatch() {
    DispatchPool::deregister();
}

fn th_runtime() -> ThRuntime {
    assert_eq!(3, ensure_min_blocking_set(3));
    ThBuilder::new()
        .name_prefix("tpool-")
        .core_threads(3)
        .blocking_threads(1024)
        .build()
        .expect("ThRuntime build")
}

fn empty_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = Tunables::default();
    let body = St::new(BodyImage::empty(), &tune);

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
    let mut rt = CtRuntime::new().unwrap();
    let task = empty_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_empty_th() {
    assert!(test_logger());
    let task = empty_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_empty_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = CtRuntime::new().unwrap();
    let task = empty_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_empty_th() {
    assert!(test_logger());
    let task = empty_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

fn small_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = Tunables::default();
    let body = St::new(BodyImage::from_slice("body"), &tune);

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
    let mut rt = CtRuntime::new().unwrap();
    let task = small_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_small_th() {
    assert!(test_logger());
    let task = small_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_small_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = CtRuntime::new().unwrap();
    let task = small_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_small_th() {
    assert!(test_logger());
    let task = small_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

fn fs_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = Tuner::new().set_buffer_size_fs(173).finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let body = St::new(in_body, &tune);

    async move {
        let mut asink = Sk::new(
            BodySink::with_fs(tune.temp_dir()).unwrap(),
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
    let mut rt = CtRuntime::new().unwrap();
    let task = fs_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_fs_th() {
    assert!(test_logger());
    let task = fs_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = CtRuntime::new().unwrap();
    let task = fs_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_th() {
    assert!(test_logger());
    let task = fs_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

fn fs_back_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = Tuner::new()
        .set_buffer_size_fs(173)
        .set_max_body_ram(15_000)
        .finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let in_body = in_body.prepare().unwrap();
    let body = St::new(in_body, &tune);

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
    let mut rt = CtRuntime::new().unwrap();
    let task = fs_back_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
fn transfer_fs_back_th() {
    assert!(test_logger());
    let task = fs_back_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

#[test]
fn transfer_fs_back_th_multi() {
    assert!(test_logger());
    let rt = th_runtime();
    for _ in 0..20 {
        let task = fs_back_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
        rt.spawn(task.map(|r: Result<(),FutioError>| r.unwrap()));
    }
    rt.shutdown_on_idle();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_back_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = CtRuntime::new().unwrap();
    let task = fs_back_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_back_th() {
    assert!(test_logger());
    let task = fs_back_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_back_th_multi() {
    assert!(test_logger());
    let rt = th_runtime();
    for _ in 0..20 {
        let task = fs_back_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
        rt.spawn(task.map(|r: Result<(),FutioError>| r.unwrap()));
    }
    rt.shutdown_on_idle();
}

#[cfg(feature = "mmap")]
fn fs_map_task<St, Sk, B>() -> impl Future<Output=Result<(), FutioError>>
    where St: StreamWrapper + TryStream + StreamExt,
          Sk: SinkWrapper<B> + Sink<B, Error=FutioError> + Unpin,
          B: From<<St as TryStream>::Ok>,
          St::Error: Into<FutioError>
{
    let tune = Tuner::new()
        .set_buffer_size_fs(173)
        .set_max_body_ram(15_000)
        .finish();
    let mut in_body = BodySink::with_fs(tune.temp_dir()).unwrap();
    in_body.write_all(vec![1; 24_000]).unwrap();
    let mut in_body = in_body.prepare().unwrap();
    in_body.mem_map().unwrap();
    let body = St::new(in_body, &tune);

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
    let mut rt = CtRuntime::new().unwrap();
    let task = fs_map_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_fs_map_th() {
    assert!(test_logger());
    let task = fs_map_task::<AsyncBodyImage, AsyncBodySink, hyper::Chunk>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_map_ct() {
    assert!(test_logger());
    register_dispatch();
    let mut rt = CtRuntime::new().unwrap();
    let task = fs_map_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let res = rt.block_on(task);
    deregister_dispatch();
    res.expect("task success");
}

#[test]
#[cfg(feature = "mmap")]
fn transfer_uni_fs_map_th() {
    assert!(test_logger());
    let task = fs_map_task::<UniBodyImage, UniBodySink, UniBodyBuf>();
    let mut rt = th_runtime();
    rt.block_on_pool(task).unwrap();
    rt.shutdown_now();
}
