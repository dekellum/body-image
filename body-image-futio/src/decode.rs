//! Implementaiton module for finding encodings and decompression

#[cfg(feature = "brotli")] use brotli;

use flate2::read::{DeflateDecoder, GzDecoder};
use hyperx::header::{
    ContentEncoding, Encoding as HyEncoding,
    Header, TransferEncoding
};
use tao_log::{debug, warn};

use body_image::{BodyImage, Dialog, Encoding, Recorded, Tunables};

use crate::FutioError;

/// Return a list of relevant encodings from the headers Transfer-Encoding and
/// Content-Encoding.  The `Chunked` encoding will be the first value if
/// found. At most one compression encoding will be the last value if found.
pub fn find_encodings(headers: &http::HeaderMap) -> Vec<Encoding> {
    let mut chunked = false;
    let mut res = Vec::with_capacity(2);

    for v in &[headers.get_all(http::header::TRANSFER_ENCODING),
               headers.get_all(http::header::CONTENT_ENCODING)] {
        match ContentEncoding::parse_header(v) {
            Ok(encs) => for av in encs.iter().rev() {
                // check in reverse, since these are in order of application
                // and we want the last
                match *av {
                    HyEncoding::Identity => {} //ignore
                    HyEncoding::Chunked  => chunked = true,
                    HyEncoding::Deflate  => res.push(Encoding::Deflate),
                    HyEncoding::Gzip     => res.push(Encoding::Gzip),
                    HyEncoding::EncodingExt(ref s) if s == "x-gzip"
                                         => res.push(Encoding::Gzip),
                    HyEncoding::Brotli   => res.push(Encoding::Brotli),
                    HyEncoding::Compress => res.push(Encoding::Compress),
                    _ => warn!("Found unknown encoding: {:?}", av),
                }
            }
            Err(e) => {
                warn!("{} on header {:?}", e, v.iter().collect::<Vec<_>>());
            }
        }
    }
    if res.len() > 1 {
        warn!("Found multiple compression encodings, \
               using first (reversed): {:?}",
              res);
        res.truncate(1);
    }
    if chunked {
        res.insert(0, Encoding::Chunked);
    }
    res
}

/// Return true if the chunked Transfer-Encoding can be found in the headers.
pub fn find_chunked(headers: &http::HeaderMap) -> bool {
    let encodings = headers.get_all(http::header::TRANSFER_ENCODING);

    for v in encodings {
        if let Ok(v) = TransferEncoding::parse_header(&v) {
            for av in v.iter() {
                if let HyEncoding::Chunked = *av { return true }
            }
        }
    }

    false
}

/// Decode the response body of the provided `Dialog` compressed with any
/// supported `Encoding`, updated the dialog accordingly.  The provided
/// `Tunables` controls decompression buffer sizes and if the final
/// `BodyImage` will be in `Ram` or `FsRead`. Returns `Ok(true)` if the
/// response body was decoded, or `Ok(false)` if no encoding was found, or an
/// error on failure, including from an unsupported `Encoding`.
pub fn decode_res_body(dialog: &mut Dialog, tune: &Tunables)
    -> Result<bool, FutioError>
{
    let mut encodings = find_encodings(dialog.res_headers());

    let compression = encodings.last().and_then(|e| {
        if *e != Encoding::Chunked { Some(*e) } else { None }
    });

    let new_body = if let Some(comp) = compression {
        debug!("Body to {:?} decode: {:?}", comp, dialog.res_body());
        Some(decompress(dialog.res_body(), comp, tune)?)
    } else {
        None
    };

    // Positively indicate that we've checked, and if necessary, successfully
    // decoded body to the associated raw Content-Type representation.
    encodings.push(Encoding::Identity);

    if let Some(b) = new_body {
        dialog.set_res_body_decoded(b, encodings);
        debug!("Body update: {:?}", dialog.res_body());
        Ok(true)
    } else {
        dialog.set_res_decoded(encodings);
        Ok(false)
    }
}

/// Decompress the provided body of any supported compression `Encoding`,
/// using `Tunables` for buffering and the final returned `BodyImage`. If the
/// encoding is not supported (e.g. `Chunked` or `Brotli`, without the feature
/// enabled), returns `Err(FutioError::UnsupportedEncoding)`.
pub fn decompress(body: &BodyImage, compression: Encoding, tune: &Tunables)
    -> Result<BodyImage, FutioError>
{
    let reader = body.reader();
    match compression {
        Encoding::Gzip => {
            let mut decoder = GzDecoder::new(reader);
            let len_est = body.len() * u64::from(tune.size_estimate_gzip());
            Ok(BodyImage::read_from(&mut decoder, len_est, tune)?)
        }
        Encoding::Deflate => {
            let mut decoder = DeflateDecoder::new(reader);
            let len_est = body.len() * u64::from(tune.size_estimate_deflate());
            Ok(BodyImage::read_from(&mut decoder, len_est, tune)?)
        }
        #[cfg(feature = "brotli")]
        Encoding::Brotli => {
            let mut decoder = brotli::Decompressor::new(
                reader,
                tune.buffer_size_ram());
            let len_est = body.len() * u64::from(tune.size_estimate_brotli());
            Ok(BodyImage::read_from(&mut decoder, len_est, tune)?)
        }
        _ => {
            Err(FutioError::UnsupportedEncoding(compression))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use body_image::{BodyError, Tuner};
    use tao_log::debugv;

    #[test]
    fn finds_chunked() {
        let mut hmap = http::HeaderMap::new();
        assert!(!find_chunked(&hmap));

        hmap.insert(http::header::TRANSFER_ENCODING,
                    "gzip".parse().unwrap());
        hmap.append(http::header::TRANSFER_ENCODING,
                    "chunked".parse().unwrap());
        assert!(find_chunked(&hmap));
    }

    #[test]
    fn finds_transfer_compression() {
        let mut hmap = http::HeaderMap::new();
        assert_eq!(find_encodings(&hmap), vec![]);

        hmap.insert(http::header::TRANSFER_ENCODING,
                    "gzip".parse().unwrap());
        hmap.append(http::header::TRANSFER_ENCODING,
                    "chunked".parse().unwrap());
        assert_eq!(
            find_encodings(&hmap),
            vec![Encoding::Chunked, Encoding::Gzip]);

        let mut hmap = http::HeaderMap::new();
        hmap.insert(http::header::TRANSFER_ENCODING,
                    "chunked".parse().unwrap());
        hmap.append(http::header::TRANSFER_ENCODING,
                    "x-gzip".parse().unwrap());
        assert_eq!(
            find_encodings(&hmap),
            vec![Encoding::Chunked, Encoding::Gzip]);

        let mut hmap = http::HeaderMap::new();
        hmap.insert(http::header::TRANSFER_ENCODING,
                    " chunked,deflate ".parse().unwrap());
        assert_eq!(
            find_encodings(&hmap),
            vec![Encoding::Chunked, Encoding::Deflate]);
    }

    #[test]
    fn finds_content_compression() {
        let mut hmap = http::HeaderMap::new();
        hmap.insert(http::header::TRANSFER_ENCODING,
                    "chunked".parse().unwrap());
        hmap.insert(http::header::CONTENT_ENCODING,
                    "br".parse().unwrap());
        assert_eq!(
            find_encodings(&hmap),
            vec![Encoding::Chunked, Encoding::Brotli]);
    }

    #[test]
    fn ignores_additional_compressions() {
        assert!(crate::logger::test_logger());

        let mut hmap = http::HeaderMap::new();
        hmap.insert(http::header::TRANSFER_ENCODING,
                    "deflate, chunked, gzip".parse().unwrap());
        hmap.insert(http::header::CONTENT_ENCODING,
                    "br".parse().unwrap());
        assert_eq!(
            find_encodings(&hmap),
            vec![Encoding::Chunked, Encoding::Gzip]);
    }

    #[test]
    fn decompress_empty_fails() {
        assert!(crate::logger::test_logger());

        let tune = Tuner::new().finish();
        let body = BodyImage::empty();

        if let Err(e @ FutioError::Body(BodyError::Io(_))) =
            decompress(&body, Encoding::Deflate, &tune)
        {
            debugv!("deflate empty", e);
        } else {
            panic!("should not succeed!");
        }

        if let Err(e @ FutioError::Body(BodyError::Io(_))) =
            decompress(&body, Encoding::Gzip, &tune)
        {
            debugv!("gunzip empty", e);
        } else {
            panic!("should not succeed!");
        }
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn decompress_empty_fails_brotli() {
        assert!(crate::logger::test_logger());

        let tune = Tuner::new().finish();
        let body = BodyImage::empty();

        if let Err(e @ FutioError::Body(BodyError::Io(_))) =
            decompress(&body, Encoding::Brotli, &tune)
        {
            debugv!("de-brotli empty", e);
        } else {
            panic!("should not succeed!");
        }
    }
}
