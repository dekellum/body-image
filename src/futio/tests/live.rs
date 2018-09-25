use ::logger::LOG_SETUP;

use futio::*;

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

    assert!(dl.res_body.is_ram());
    assert!(dl.res_body.len() > 0);
}

#[test]
fn test_small_https() {
    assert!(*LOG_SETUP);
    let tune = Tunables::new();
    let req = get_request("https://www.usa.gov").unwrap();

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
    let req = get_request("http://gravitext.com/no/existe").unwrap();

    let dl = fetch(req, &tune).unwrap();
    println!("Response {:#?}", dl);

    assert_eq!(dl.status.as_u16(), 404);

    assert!(dl.res_body.is_ram());
    assert!(dl.res_body.len() > 0);
    assert!(dl.res_body.len() < 1000);
}
