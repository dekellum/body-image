extern crate failure;
extern crate http;
extern crate hyper_barc;

use failure::Error as FlError;

use hyper_barc::{HyBody, HyperBowl};
use hyper_barc::barc::BarcFile;
use hyper_barc::compress::decode_body;

fn main() {
    let mut args = std::env::args();
    args.next(); //$0
    let url = args.next().expect("URL argument required");
    let barc_path = args.next().expect("BARC-FILE argument required");

    run(&url, &barc_path).expect("barc-record");
}

fn run(url: &str, barc_path: &str) -> Result<(), FlError> {
    let req = http::Request::builder()
        .method(http::Method::GET)
        .header(http::header::ACCEPT,
                "text/html, application/xhtml+xml, \
                 application/xml;q=0.9, \
                 */*;q=0.8" )
        .header(http::header::ACCEPT_LANGUAGE, "en")
        .header(http::header::ACCEPT_ENCODING, "gzip, deflate")
        .header(http::header::USER_AGENT,
                "Mozilla/5.0 \
                 (compatible; Hyper-bowl 0.0.1; +http://gravitext.com/)")
        // "Connection: keep-alive" (header) is default for HTTP 1.1
        .uri(url)
        .body(HyBody::empty())?;

    let hb = HyperBowl::new()?;
    let mut dl = hb.fetch(req)?;

    decode_body(&mut dl)?;

    dl = dl.map_if_fs()?;

    let bfile = BarcFile::open(barc_path)?;
    let mut bw = bfile.writer()?;
    bw.write(&dl)?;
    Ok(())
}
