use std::convert::TryFrom;

use barc::{BarcFile, CompressStrategy, Record};
use body_image::Tunables;
use body_image_futio::{
    ACCEPT_ENCODINGS, BROWSE_ACCEPT, decode_res_body, fetch,
    FutioError, RequestRecord, RequestRecorder, user_agent
};

use crate::{Flaw, quit};

/// The `record` command implementation.
pub(crate) fn record(
    url: &str,
    barc_path: &str,
    decode: bool,
    accept: Option<&str>,
    strategy: &dyn CompressStrategy)
    -> Result<(), Flaw>
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
        match decode_res_body(&mut dialog, &tune) {
            Ok(_) => {}
            Err(FutioError::UnsupportedEncoding(enc)) => {
                quit!("Unsupported encoding {}; \
                       use `--no-decode` flag to record anyway",
                      enc);
            }
            Err(e) => return Err(e.into())
        }
    }

    let bfile = BarcFile::new(barc_path);
    let mut bw = bfile.writer()?;

    let record = Record::try_from(dialog)?;

    bw.write(&record, strategy)?;
    Ok(())
}
