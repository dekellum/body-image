extern crate hyper_stub;

use std::io;
use std::time::Duration;

use ::logger::LOG_SETUP;
use ::Tuner;

use async::*;

use async::hyper::client::connect::Connect;
use async::hyper::Client;

#[test]
fn test_small_get() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let rq = get_request("http://gravitext.com/stubs/no/existe");
    let client = hyper_stub::proxy_client_fn_ok(|_req| {
        hyper::Response::new("stub".into())
    });
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(dl) => {
            assert_eq!(dl.res_body.len(), 4);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn test_small_post() {
    assert!(*LOG_SETUP);
    let tune = Tunables::default();
    let body = BodyImage::from_slice(&b"stub"[..]);
    let rq: RequestRecord<hyper::Body> = post_request(body, &tune);
    let client = echo_server_stub();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(dl) => {
            assert_eq!(dl.res_body.len(), 4);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn test_large_post() {
    assert!(*LOG_SETUP);
    let tune = Tunables::default();
    let body = fs_body_image(194_767);
    let rq = post_request(body, &tune);
    let client = echo_server_stub();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(dl) => {
            assert_eq!(dl.res_body.len(), 194_767);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn test_large_concurrent() {
    assert!(*LOG_SETUP);

    let mut pool = tokio::executor::thread_pool::Builder::new();
    pool.name_prefix("tpool-")
        .pool_size(1)
        .max_blocking(2);
    let mut rt = tokio::runtime::Builder::new()
        .threadpool_builder(pool)
        .build().unwrap();

    let rq0 = get_request("http://foo.com/r1");
    let rq1 = get_request("http://foo.com/r2");

    let client = {
        hyper_stub::proxy_client_fn_ok(move |req| {
            let tune = Tuner::new().set_buffer_size_fs(15_999).finish();
            let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
            if req.uri() == "http://foo.com/r1" {
                body.write_all(vec![b'0';  74_333]).unwrap();
            } else {
                body.write_all(vec![b'1'; 193_400]).unwrap();
            }
            let body = AsyncBodyImage::new(body.prepare().unwrap(), &tune);
            hyper::Response::new(hyper::Body::wrap_stream(body))
        })
    };

    let tune = Tuner::new()
        .set_max_body_ram(64 * 1024)
        .finish();
    let res = rt.block_on(
        request_dialog(&client, rq0, &tune)
            .join(request_dialog(&client, rq1, &tune))
    );
    match res {
        Ok((dl0, dl1)) => {
            assert_eq!(dl0.res_body.len(),  74_333);
            assert_eq!(dl1.res_body.len(), 193_400);
            assert!(!dl0.res_body.is_ram());
            assert!(!dl1.res_body.is_ram());
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn test_timeout_pre() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .set_res_timeout(Duration::from_millis(10))
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    let rq: RequestRecord<hyper::Body> = get_request("http://foo.com/");
    let client = delayed_server();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => {
            let em = e.to_string();
            assert!(em.starts_with("timeout"), em);
            assert!(em.contains("initial"), em);
        }
    }
}

#[test]
fn test_timeout_streaming() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .unset_res_timeout() //workaround, see *_race version below
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    let rq: RequestRecord<hyper::Body> = get_request("http://foo.com/");
    let client = delayed_server();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => {
            let em = e.to_string();
            assert!(em.starts_with("timeout"), em);
            assert!(em.contains("streaming"), em);
        }
    }
}

#[test]
#[cfg(feature = "may_fail")]
fn test_timeout_streaming_race() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        // Correct test assertions, but this may fail on CI due to timing
        // issues
        .set_res_timeout(Duration::from_millis(590))
        .set_body_timeout(Duration::from_millis(600))
        .finish();
    let rq: RequestRecord<hyper::Body> = get_request("http://foo.com/");
    let client = delayed_server();
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(_) => {
            panic!("should have timed-out!");
        }
        Err(e) => {
            let em = e.to_string();
            assert!(em.starts_with("timeout"), em);
            assert!(em.contains("streaming"), em);
        }
    }
}

fn fs_body_image(size: usize) -> BodyImage {
    let tune = Tunables::default();
    let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
    body.write_all(vec![1; size]).unwrap();
    body.prepare().unwrap()
}

fn ram_body_image(csize: usize, count: usize) -> BodyImage {
    let mut bs = BodySink::with_ram_buffers(count);
    for _ in 0..count {
        bs.save(vec![1; csize]).expect("safe for Ram");
    }
    bs.prepare().expect("safe for Ram")
}

fn get_request(url: &str) -> RequestRecord<hyper::Body>
{
    http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header(http::header::USER_AGENT, &user_agent()[..])
        .record()
        .expect("get_request")
}

fn post_request<T>(body: BodyImage, tune: &Tunables) -> RequestRecord<T>
    where T: hyper::body::Payload + Send,
          http::request::Builder: RequestRecorder<T>
{
    http::Request::builder()
        .method(http::Method::POST)
        .uri("http://foobar.com")
        .header(http::header::USER_AGENT, &user_agent()[..])
        // To avoid chunked transfer
        // .header(http::header::CONTENT_LENGTH, body.len())
        .record_body_image(body, &tune)
        .expect("post_request")
}

/// A `Client` stub to a body echo'ing server, which buffers complete requests
/// (potentially to disk) using AsyncBodySink and responds with AsyncBodyImage
fn echo_server_stub() -> Client<impl Connect> {
    hyper_stub::proxy_client_fn(|req| {
        let tune = Tuner::new()
            .set_buffer_size_fs(2734)
            .set_max_body_ram(15_000)
            .finish();
        let asink = AsyncBodySink::new(
            BodySink::with_ram_buffers(4),
            tune
        );
        req.into_body()
            .from_err::<Flare>()
            .forward(asink)
            .and_then(move |(_strm, asink)| {
                let tune = Tuner::new().set_buffer_size_fs(4972).finish();
                let bi = asink.into_inner().prepare()?;
                Ok(http::Response::builder()
                   .status(200)
                   .body(hyper::Body::wrap_stream(
                       AsyncBodyImage::new(bi, &tune)
                   ))?
                )
            })
            .map_err(|e| e.compat())
    })
}

/// A `Client` stub to a server always returning a 1 MiB response body, after
/// delaying before the initial response, and before completing the body. For
/// testing timeouts.
fn delayed_server() -> Client<impl Connect> {
    hyper_stub::proxy_client_fn(|_req| {
        let now = Instant::now();
        let delay1 = tokio::timer::Delay::new(now + Duration::from_millis(100))
            .map_err(|e| -> http::Error { unreachable!(e) });
        let delay2 = tokio::timer::Delay::new(now + Duration::from_millis(900))
            .map_err(|e| -> io::Error { unreachable!(e) });
        let bi = ram_body_image(0x8000, 32);
        delay1.and_then(move |()| {
            let tune = Tunables::default();
            future::result(http::Response::builder().status(200).body(
                hyper::Body::wrap_stream(
                    AsyncBodyImage::new(bi, &tune).select(
                        delay2
                            .map(|_| Bytes::new())
                            .into_stream()
                    )
                )
            ))
        })
    })
}
