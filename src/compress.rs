//! Compression and decompression support functions.

extern crate flate2;

use super::failure::Error as FlError;
use self::flate2::read::{DeflateDecoder, GzDecoder};
use hyper::header::{ContentEncoding, Encoding, Header, Raw};
use super::http;
use super::{BodyImage, Dialog, META_RES_DECODED, Tunables};

/// Decode any _gzip_ or _deflate_ response Transfer-Encoding or
/// Content-Encoding into a new response `BodyItem`, updating `Dialog`
/// accordingly. The provided `Tunables` controls decompression buffer
/// sizes and if the final `BodyItem` will be in `Ram` or `FsRead`.
pub fn decode_res_body(dialog: &mut Dialog, tune: &Tunables)
    -> Result<(), FlError>
{
    let headers = &dialog.res_headers;
    let encodings = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .chain(headers
               .get_all(http::header::CONTENT_ENCODING)
               .iter());

    let mut chunked = false;
    let mut compress = None;

    'headers: for v in encodings {
        // Hyper's Content-Encoding includes Brotli (br) _and_
        // Chunked, is thus a super-set of Transfer-Encoding, so parse
        // all of these headers that way.
        if let Ok(v) = ContentEncoding::parse_header(&Raw::from(v.as_bytes())) {
            for av in v.iter() {
                match *av {
                    Encoding::Chunked => chunked = true,
                    Encoding::Gzip | Encoding::Deflate => { // supported
                        compress = Some(av.clone());
                        break 'headers;
                    }
                    Encoding::Identity => (),
                    _ => {
                        println!("Unsupported Encoding for decode: {:?}", av);
                        break 'headers;
                    }
                }
            }
        }
    }

    if let Some(ref comp) = compress {
        dialog.res_body = {
            println!("Body to {:?} decode: {:?}", comp, dialog.res_body);
            let mut reader = dialog.res_body.reader();
            match *comp {
                Encoding::Gzip => {
                    let mut decoder = GzDecoder::new(reader.as_read());
                    let len_est = dialog.res_body.len() *
                        u64::from(tune.size_estimate_gzip());
                    BodyImage::read_from(&mut decoder, len_est, tune)?
                }
                Encoding::Deflate => {
                    let mut decoder = DeflateDecoder::new(reader.as_read());
                    let len_est = dialog.res_body.len() *
                        u64::from(tune.size_estimate_deflate());
                    BodyImage::read_from(&mut decoder, len_est, tune)?
                }
                _ => unreachable!("Not supported: {:?}", comp)
            }
        };
        println!("Body update: {:?}", dialog.res_body);
    }

    if chunked || compress.is_some() {
        let mut ds = Vec::with_capacity(2);
        if chunked {
            ds.push(Encoding::Chunked.to_string())
        }
        if let Some(ref e) = compress {
            ds.push(e.to_string())
        }
        dialog.meta.append(http::header::HeaderName
                           ::from_lowercase(META_RES_DECODED).unwrap(),
                           ds.join(", ").parse()?);
    }
    Ok(())
}
