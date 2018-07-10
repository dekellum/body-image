use ::std::time::Duration;
use ::logger::LOG_SETUP;
use ::Tuner;

use async::*;

fn create_request(url: &str)
    -> Result<RequestRecord<hyper::Body>, Flare>
{
    http::Request::builder()
        .method(http::Method::GET)
        .header(http::header::ACCEPT, BROWSE_ACCEPT)
        .header(http::header::ACCEPT_LANGUAGE, "en")
        .header(http::header::ACCEPT_ENCODING, ACCEPT_ENCODINGS)
        .header(http::header::USER_AGENT, &user_agent()[..])
        .uri(url)
        .record()
}

#[test]
fn test_small_http() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = create_request("http://gravitext.com").unwrap();

    let dl = fetch(req, &tune).unwrap();
    println!("Response {:#?}", dl);

    assert!(dl.res_body.is_ram());
    assert!(dl.res_body.len() > 0);
}

#[test]
fn test_small_https() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = create_request("https://www.usa.gov").unwrap();

    let dl = fetch(req, &tune).unwrap();
    let dl = dl.clone();
    println!("Response {:#?}", dl);

    assert!(dl.res_body.is_ram());
    assert!(dl.res_body.len() > 0);
}

#[test]
fn test_not_found() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = create_request("http://gravitext.com/no/existe").unwrap();

    let dl = fetch(req, &tune).unwrap();
    println!("Response {:#?}", dl);

    assert_eq!(dl.status.as_u16(), 404);

    assert!(dl.res_body.is_ram());
    assert!(dl.res_body.len() > 0);
    assert!(dl.res_body.len() < 1000);
}

#[test]
fn test_large_http() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .set_max_body_ram(64 * 1024)
        .finish();
    let req = create_request(
        "http://gravitext.com/images/jakarta_slum.jpg"
    ).unwrap();
    let dl = fetch(req, &tune).unwrap();
    println!("Response {:#?}", dl);

    assert!(dl.res_body.len() > (64 * 1024));
    assert!(!dl.res_body.is_ram());
}

#[test]
fn test_large_parallel_constrained() {
    assert!(*LOG_SETUP);
    let tune = Tuner::new()
        .set_max_body_ram(64 * 1024)
        .set_res_timeout(Duration::from_secs(15))
        .set_body_timeout(Duration::from_secs(55))
        .finish();

    let mut pool = tokio::executor::thread_pool::Builder::new();
    pool.name_prefix("tpool-")
        .pool_size(1)
        .max_blocking(2);
    let mut rt = tokio::runtime::Builder::new()
        .threadpool_builder(pool)
        .build().unwrap();

    let client = hyper::Client::new();

    let rq0 = create_request(
        "http://cache.ruby-lang.org/pub/ruby/1.8/ChangeLog-1.8.2"
    ).unwrap();
    let rq1 = create_request(
        "http://cache.ruby-lang.org/pub/ruby/1.8/ChangeLog-1.8.3"
    ).unwrap();

    let res = rt.block_on(
        request_dialog(&client, rq0, &tune)
            .join(request_dialog(&client, rq1, &tune))
    );
    match res {
        Ok((dl0, dl1)) => {
            assert_eq!(dl0.res_body.len(), 333_210);
            assert_eq!(dl1.res_body.len(), 134_827);
            assert!(!dl0.res_body.is_ram());
            assert!(!dl1.res_body.is_ram());
        }
        Err(e) => {
            panic!("failed with: {}", e);
        }
    }
}
