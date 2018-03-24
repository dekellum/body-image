extern crate failure;
extern crate http;
extern crate hyper_bowl;

use std::process;

use failure::Error as FlError;

use hyper_bowl::{fetch, RequestRecordable, Tunables};
use hyper_bowl::barc::BarcFile;
use hyper_bowl::compress::decode_res_body;

fn main() {
    let mut args = std::env::args();
    args.next(); //$0
    let url = args.next().expect("URL argument required");
    let barc_path = args.next().expect("BARC-FILE argument required");

    let r = run(&url, &barc_path);
    if let Err(e) = r {
        eprintln!("Error cause: {}; (Backtrace) {}", e.cause(), e.backtrace());
        process::exit(2);
    }
}

fn run(url: &str, barc_path: &str) -> Result<(), FlError> {
    let req = http::Request::builder()
        .method(http::Method::GET)
        .header(http::header::ACCEPT,
                "text/html, application/xhtml+xml, \
                 application/xml;q=0.9, \
                 */*;q=0.8" )
        .header(http::header::ACCEPT_LANGUAGE, "en")
        .header(http::header::ACCEPT_ENCODING, "br, gzip, deflate")
        .header(http::header::USER_AGENT,
                "Mozilla/5.0 \
                 (compatible; hyper-bowl 0.1.0; \
                  +http://github.com/dekellum/hyper-bowl)")
        // "Connection: keep-alive" (header) is default for HTTP 1.1
        .uri(url)
        .record()?;

    let tune = Tunables::new();
    let mut dl = fetch(req, &tune)?;

    decode_res_body(&mut dl, &tune)?;
    dl.res_map()?;

    let bfile = BarcFile::new(barc_path);
    let mut bw = bfile.writer()?;
    bw.write(&dl)?;
    Ok(())
}
