use failure::Error as Flare;
use body_image::Tunables;
use body_image::barc::{BarcFile, CompressStrategy};
use body_image::client::{ACCEPT_ENCODINGS, BROWSE_ACCEPT,
                         decode_res_body, fetch,
                         user_agent, RequestRecordable};
use http;

pub(crate) fn record(url: &str, barc_path: &str, strategy: &CompressStrategy)
    -> Result<(), Flare>
{
    let req = http::Request::builder()
        .method(http::Method::GET)
        .header(http::header::ACCEPT, BROWSE_ACCEPT)
        .header(http::header::ACCEPT_LANGUAGE, "en")
        .header(http::header::ACCEPT_ENCODING, ACCEPT_ENCODINGS)
        .header(http::header::USER_AGENT, &user_agent()[..])
        .uri(url)
        .record()?;

    let tune = Tunables::new();
    let mut dl = fetch(req, &tune)?;

    decode_res_body(&mut dl, &tune)?;

    let bfile = BarcFile::new(barc_path);
    let mut bw = bfile.writer()?;
    bw.write(&dl, strategy)?;
    Ok(())
}
