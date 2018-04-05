use failure::Error as Flare;
use body_image::Tunables;
use body_image::barc::{BarcFile, CompressStrategy};
use body_image::client::{decode_res_body, fetch, RequestRecordable};
use http;

pub(crate) fn record(url: &str, barc_path: &str, strategy: &CompressStrategy)
    -> Result<(), Flare>
{
    let encodings = if cfg!(feature = "brotli") {
        "br, gzip, deflate"
    } else {
        "gzip, deflate"
    };

    let req = http::Request::builder()
        .method(http::Method::GET)
        .header(http::header::ACCEPT,
                "text/html, application/xhtml+xml, \
                 application/xml;q=0.9, \
                 */*;q=0.8" )
        .header(http::header::ACCEPT_LANGUAGE, "en")
        .header(http::header::ACCEPT_ENCODING, encodings)
        .header(http::header::USER_AGENT,
                "Mozilla/5.0 \
                 (compatible; body-image 0.1.0; \
                  +http://github.com/dekellum/body-image)")
        // "Connection: keep-alive" (header) is default for HTTP 1.1
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
