use tao_log::debugv;

use body_image::Recorded;

use crate::{
    ACCEPT_ENCODINGS, BROWSE_ACCEPT,
    fetch, FutioTunables,
    RequestRecord, RequestRecorder, user_agent
};
use piccolog::test_logger;

fn get_request(url: &str)
    -> Result<RequestRecord<hyper::Body>, http::Error>
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
    assert!(test_logger());
    let tune = FutioTunables::new();
    let req = get_request("http://gravitext.com").unwrap();

    let dl = debugv!(fetch(req, tune)).unwrap();

    assert!(dl.res_body().is_ram());
    assert!(!dl.res_body().is_empty());
}

#[test]
fn test_small_https() {
    assert!(test_logger());
    let tune = FutioTunables::new();
    let req = get_request("https://www.usa.gov").unwrap();

    let dl = debugv!(fetch(req, tune)).unwrap();
    let dl = dl.clone(); // for coverage only

    assert!(dl.res_body().is_ram());
    assert!(!dl.res_body().is_empty());
}

#[test]
fn test_not_found() {
    assert!(test_logger());
    let tune = FutioTunables::new();
    let req = get_request("http://gravitext.com/no/existe").unwrap();

    let dl = debugv!(fetch(req, tune)).unwrap();

    assert_eq!(dl.res_status().as_u16(), 404);

    assert!(dl.res_body().is_ram());
    assert!(!dl.res_body().is_empty());
    assert!(dl.res_body().len() < 1000);
}
