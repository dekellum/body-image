use failure::Error as Flare;

use body_image::Tunables;
use crate::{ACCEPT_ENCODINGS, BROWSE_ACCEPT, fetch, request_dialog,
            Recorded, RequestRecord, RequestRecorder, user_agent};
use crate::logger::LOG_SETUP;

fn get_request(url: &str)
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
    let req = get_request("http://gravitext.com").unwrap();

    let dl = fetch(req, &tune).unwrap();
    println!("Response {:#?}", dl);

    assert!(dl.res_body().is_ram());
    assert!(dl.res_body().len() > 0);
}

#[test]
fn test_small_https() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = get_request("https://www.usa.gov").unwrap();

    let dl = fetch(req, &tune).unwrap();
    let dl = dl.clone();
    println!("Response {:#?}", dl);

    assert!(dl.res_body().is_ram());
    assert!(dl.res_body().len() > 0);
}

#[test]
fn test_http_2() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = get_request("https://abc.xyz/").unwrap();

    let mut rt = tokio::runtime::Builder::new()
        .name_prefix("tpool-")
        .core_threads(2)
        .blocking_threads(2)
        .build()
        .unwrap();

    use hyper::client::connect::HttpConnector;
    use hyper_openssl::HttpsConnector;
    use openssl::ssl::{SslMethod, SslConnector};

    let mut ssl = SslConnector::builder(SslMethod::tls()).unwrap();
    ssl.set_alpn_protos(b"\x02h2").unwrap();
    let mut httpc = HttpConnector::new(2);
    httpc.enforce_http(false);
    let connector = HttpsConnector::with_connector(httpc, ssl).unwrap();

    let client = hyper::Client::builder().http2_only(true).build(connector);
    let dl = rt.block_on(request_dialog(&client, req, &tune)).unwrap();

    println!("Response {:#?}", dl);

    assert!(dl.res_body().is_ram());
    assert!(dl.res_body().len() > 0);
    assert_eq!(dl.res_version(), http::Version::HTTP_2);
}

#[test]
fn test_not_found() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = get_request("http://gravitext.com/no/existe").unwrap();

    let dl = fetch(req, &tune).unwrap();
    println!("Response {:#?}", dl);

    assert_eq!(dl.res_status().as_u16(), 404);

    assert!(dl.res_body().is_ram());
    assert!(dl.res_body().len() > 0);
    assert!(dl.res_body().len() < 1000);
}
