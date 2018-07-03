extern crate hyper_stub;

use ::logger::LOG_SETUP;
use ::Tuner;

use client::*;

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
    body.write_all(
       "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
        sed do eiusmod tempor incididunt ut labore et dolore magna \
        aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
        ullamco laboris nisi ut aliquip ex ea commodo \
        consequat. Duis aute irure dolor in reprehenderit in \
        voluptate velit esse cillum dolore eu fugiat nulla \
        pariatur. Excepteur sint occaecat cupidatat non proident, \
        sunt in culpa qui officia deserunt mollit anim id est \
        laborum."
    ).unwrap();
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
fn test_byte_post() {
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
