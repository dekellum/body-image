extern crate hyper;

use failure::Error as Flare;
use body_image::Tunables;
use body_image::barc::{BarcFile, CompressStrategy, Record};
use body_image::client::{ACCEPT_ENCODINGS, BROWSE_ACCEPT,
                         decode_res_body, fetch,
                         user_agent, RequestRecord, RequestRecordableEmpty};
use http;

/// The `record` command implementation.
pub(crate) fn record(
    url: &str,
    barc_path: &str,
    decode: bool,
    accept: Option<&str>,
    strategy: &CompressStrategy)
    -> Result<(), Flare>
{
    let req: RequestRecord<hyper::Body> = http::Request::builder()
        .method(http::Method::GET)
        .header(http::header::ACCEPT, accept.unwrap_or(BROWSE_ACCEPT))
        .header(http::header::ACCEPT_LANGUAGE, "en")
        .header(http::header::ACCEPT_ENCODING, ACCEPT_ENCODINGS)
        .header(http::header::USER_AGENT, user_agent().as_str())
        .uri(url)
        .record()?;

    let tune = Tunables::new();
    let mut dialog = fetch(req, &tune)?;

    if decode {
        decode_res_body(&mut dialog, &tune)?;
    }

    let bfile = BarcFile::new(barc_path);
    let mut bw = bfile.writer()?;

    let record = Record::try_from(dialog)?;

    bw.write(&record, strategy)?;
    Ok(())
}
