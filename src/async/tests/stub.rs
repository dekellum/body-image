extern crate hyper_stub;

use ::logger::LOG_SETUP;
use ::Tuner;

use async::*;

fn get_request(url: &str)
    -> Result<RequestRecord<hyper::Body>, Flare>
{
    http::Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .record()
}

#[test]
fn test_small_dialog() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let rq = get_request(
        "http://gravitext.com/stubs/no/existe"
    ).unwrap();
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
fn test_fs_image_post() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .set_buffer_size_fs(17)
        .finish();
    let mut body = BodySink::with_fs(tune.temp_dir()).unwrap();
    body.write_all(vec![b'a'; 445]).unwrap();
    let body = body.prepare().unwrap();

    let rq: RequestRecord<hyper::Body> = http::Request::builder()
        .method(http::Method::POST)
        .header(http::header::USER_AGENT, &user_agent()[..])
        // To avoid 27 chunks
        // .header(http::header::CONTENT_LENGTH, "445")
        .uri("http://foobar.com")
        .record_body_image(body, &tune)
        .unwrap();
    let client = hyper_stub::proxy_client_fn_ok(|req| {
        hyper::Response::new(req.into_body())
    });
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(request_dialog(&client, rq, &tune));
    match res {
        Ok(dl) => {
            println!("{:#?}", dl);
            assert_eq!(dl.res_body.len(), 445);
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}

#[test]
fn test_small_post() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let rq: RequestRecord<hyper::Body> = http::Request::builder()
        .method(http::Method::POST)
        .uri("http://foobar.com")
        .record_body(&b"stub"[..])
        .unwrap();
    let client = hyper_stub::proxy_client_fn_ok(|req| {
        hyper::Response::new(req.into_body())
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
fn test_large_concurrent_constrained() {
    assert!(*LOG_SETUP);

    let mut pool = tokio::executor::thread_pool::Builder::new();
    pool.name_prefix("tpool-")
        .pool_size(1)
        .max_blocking(2);
    let mut rt = tokio::runtime::Builder::new()
        .threadpool_builder(pool)
        .build().unwrap();

    let rq0 = get_request("http://foo.com/r1").unwrap();
    let rq1 = get_request("http://for.com/r2").unwrap();

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
