//! BARC record compression support

use std::fs::File;
use std::io;
use std::io::{Read, Write};

use flate2::Compression as GzCompression;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use http;
use http::header::HeaderName;
use tao_log::{debug, trace};
use olio::fs::rc::ReadSlice;

#[cfg(feature = "brotli")]
use brotli;

use crate::{ BarcError, MetaRecorded, hname_meta_res_decoded };

/// BARC record compression mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Compression {
    /// Not compressed.
    Plain,

    /// Compressed with gzip.
    Gzip,

    /// Compressed with brotli.
    Brotli,
}

impl Compression {
    /// Return (char) flag for variant.
    pub(crate) fn flag(self) -> char {
        match self {
            Compression::Plain   => 'P',
            Compression::Gzip    => 'Z',
            Compression::Brotli  => 'B',
        }
    }

    /// Return variant for (byte) flag, or fail.
    pub(crate) fn from_byte(f: u8) -> Result<Self, BarcError> {
        match f {
            b'P' => Ok(Compression::Plain),
            b'Z' => Ok(Compression::Gzip),
            b'B' => Ok(Compression::Brotli),
            _ => Err(BarcError::UnknownCompression(f))
        }
    }
}

/// Strategies for BARC record compression encoding on write.
pub trait CompressStrategy {
    /// Return an `EncodeWrapper` for `File` by evaluating the
    /// `MetaRecorded` for compression worthiness.
    fn wrap_encoder<'a>(&self, rec: &dyn MetaRecorded, file: &'a File)
        -> Result<EncodeWrapper<'a>, BarcError>;

    /// Return minimum length of compressible bytes for compression.
    fn min_len(&self) -> u64 { 0 }

    /// Return a coefficient used to weight the discount of non-compressible
    /// body bytes. Default: 0.5
    fn non_compressible_coef(&self) -> f64 { 0.5 }

    /// Return whether to check the meta -decoded header for an "identity"
    /// value, as proof that the content-type header actually characterizes
    /// the associated body, for the purpose of counting compressible bytes.
    /// Default: false (may change in the future)
    fn check_identity(&self) -> bool { false }

    /// Return true if the provided record has at least `min_len` of
    /// compressible bytes, from the response and request bodies and
    /// headers.
    ///
    /// Any non-compressible bytes, from non-compressible bodies, are
    /// discounted, weighted by `non_compressible_coef`.
    #[allow(unused_parens)]
    fn is_compressible(&self, rec: &dyn MetaRecorded) -> bool {
        static CT: HeaderName = http::header::CONTENT_TYPE;
        let mut clen = 0;
        let mut min_len = self.min_len();
        let mut sufficient = None;

        let len = rec.res_body().len();
        if len > 0 {
            if ((!self.check_identity() ||
                 is_identity(rec.meta().get(hname_meta_res_decoded()))) &&
                is_compressible_type(rec.res_headers().get(&CT))) {
                clen += len;
            } else {
                min_len = min_len.saturating_add(
                    (len as f64 * self.non_compressible_coef()) as u64
                );
            }
        }

        let len = rec.req_body().len();
        if len > 0 {
            if is_compressible_type(rec.req_headers().get(&CT)) {
                clen += len;
            } else {
                min_len = min_len.saturating_add(
                    (len as f64 * self.non_compressible_coef()) as u64
                );
            }
        }

        if clen >= min_len { sufficient = Some("bodies"); }

        if sufficient.is_none() {
            clen += len_of_headers(rec.res_headers()) as u64;
            if clen >= min_len { sufficient = Some("res_headers"); }
        }

        if sufficient.is_none() {
            clen += len_of_headers(rec.req_headers()) as u64;
            if clen >= min_len { sufficient = Some("req_headers"); }
        }

        if sufficient.is_none() {
            clen += len_of_headers(rec.meta()) as u64;
            if clen >= min_len { sufficient = Some("meta (all)"); }
        }

        if let Some(s) = sufficient {
            debug!("found sufficient compressible length {} >= {}, at {}",
                   clen, min_len, s);
            true
        } else {
            debug!("compressible length {} < {}, won't compress",
                   clen, min_len);
            false
        }
    }
}

/// Strategy of no (aka `Plain`) compression.
#[derive(Clone, Copy, Debug)]
pub struct NoCompressStrategy {}

impl Default for NoCompressStrategy {
    fn default() -> Self { Self {} }
}

impl CompressStrategy for NoCompressStrategy {
    /// Return an `EncodeWrapper` for `File`. This implementation
    /// always returns a `Plain` wrapper.
    fn wrap_encoder<'a>(&self, _rec: &dyn MetaRecorded, file: &'a File)
        -> Result<EncodeWrapper<'a>, BarcError>
    {
        Ok(EncodeWrapper::plain(file))
    }
}

/// Strategy for gzip compression. Will only compress if a minimum length of
/// compressible bytes, from the response and request bodies and headers is
/// found.
#[derive(Clone, Copy, Debug)]
pub struct GzipCompressStrategy {
    min_len: u64,
    compression_level: u32,
    check_identity: bool,
}

impl GzipCompressStrategy {
    /// Set minimum length of compressible bytes required to use compression.
    /// Default: 4 KiB.
    pub fn set_min_len(mut self, size: u64) -> Self {
        self.min_len = size;
        self
    }

    /// Set the compression level to use, typically on a scale of 0-9
    /// where 0 is _no compression_ and 9 is highest (and slowest)
    /// compression. Default: 6.
    pub fn set_compression_level(mut self, level: u32) -> Self {
        self.compression_level = level;
        self
    }

    /// Set whether to check the meta -decoded header for an "identity" value,
    /// as proof that the content-type header actually characterizes the
    /// associated body.
    ///
    /// For example, `body_image_futio::decode_res_body` as of crate version
    /// 1.1.0, will set this value on an original `Dialog`, which is preserved
    /// when converted to a `Record` for barc write.
    /// Default: false (may change in the future)
    pub fn set_check_identity(mut self, check: bool) -> Self {
        self.check_identity = check;
        self
    }
}

impl Default for GzipCompressStrategy {
    fn default() -> Self {
        Self { min_len: 4 * 1024,
               compression_level: 6,
               check_identity: false }
    }
}

impl CompressStrategy for GzipCompressStrategy {
    fn wrap_encoder<'a>(&self, rec: &dyn MetaRecorded, file: &'a File)
        -> Result<EncodeWrapper<'a>, BarcError>
    {
        if self.is_compressible(rec) {
            Ok(EncodeWrapper::gzip(file, self.compression_level))
        } else {
            Ok(EncodeWrapper::plain(file))
        }
    }

    fn min_len(&self) -> u64 {
        self.min_len
    }

    fn check_identity(&self) -> bool {
        self.check_identity
    }
}

/// Strategy for Brotli compression. Will only compress if a minimum length of
/// compressible bytes, from the response and request bodies and headers is
/// found.
#[cfg(feature = "brotli")]
#[derive(Clone, Copy, Debug)]
pub struct BrotliCompressStrategy {
    min_len: u64,
    compression_level: u32,
    check_identity: bool,
}

#[cfg(feature = "brotli")]
impl BrotliCompressStrategy {
    /// Set minimum length of compressible bytes required to use compression.
    /// Default: 1 KiB.
    pub fn set_min_len(mut self, size: u64) -> Self {
        self.min_len = size;
        self
    }

    /// Set the compression level to use, typically on a scale of 0-9
    /// where 0 is _no compression_ and 9 is highest (and slowest)
    /// compression. Default: 6.
    pub fn set_compression_level(mut self, level: u32) -> Self {
        self.compression_level = level;
        self
    }

    /// Set whether to check the meta -decoded header for an "identity" value,
    /// as proof that the content-type header actually characterizes the
    /// associated body.
    ///
    /// For example, `body_image_futio::decode_res_body` as of crate version
    /// 1.1.0, will set this value on an original `Dialog`, which is preserved
    /// when converted to a `Record` for barc write.
    /// Default: false (may change in the future)
    pub fn set_check_identity(mut self, check: bool) -> Self {
        self.check_identity = check;
        self
    }
}

#[cfg(feature = "brotli")]
impl Default for BrotliCompressStrategy {
    fn default() -> Self {
        Self { min_len: 1024,
               compression_level: 6,
               check_identity: false }
    }
}

#[cfg(feature = "brotli")]
impl CompressStrategy for BrotliCompressStrategy {
    fn wrap_encoder<'a>(&self, rec: &dyn MetaRecorded, file: &'a File)
        -> Result<EncodeWrapper<'a>, BarcError>
    {
        if self.is_compressible(rec) {
            Ok(EncodeWrapper::brotli(file, self.compression_level))
        } else {
            Ok(EncodeWrapper::plain(file))
        }
    }

    fn min_len(&self) -> u64 {
        self.min_len
    }

    fn check_identity(&self) -> bool {
        self.check_identity
    }
}

// Return true if the meta -decoded header end's with "identity", confirming
// that associated body matches the content-type (no intervening compression).
fn is_identity(decoded: Option<&http::header::HeaderValue>) -> bool {
    static IDY: &[u8] = b"identity";

    if let Some(hv) = decoded {
        let hvb = hv.as_bytes();
        if hvb.len() >= IDY.len() && &hvb[(hvb.len()-IDY.len())..] == IDY {
            return true;
        }
    }
    false
}

// Return true if the given content-type header value is expected to be
// compressible, e.g. "text/html", "image/svg", etc.
fn is_compressible_type(ctype: Option<&http::header::HeaderValue>) -> bool {
    if let Some(ctype_v) = ctype {
        if let Ok(ctype_str) = ctype_v.to_str() {
            is_compressible_type_str(ctype_str)
        } else {
            debug!("not compressible: content-type header not utf-8");
            false
        }
    } else {
        trace!("not compressible: no content-type header");
        false
    }
}

fn is_compressible_type_str(ctype_str: &str) -> bool {
    match ctype_str.trim().parse::<mime::Mime>() {
        Ok(mtype) => match (mtype.type_(), mtype.subtype()) {
            (mime::TEXT, _)                           => true,
            (mime::APPLICATION, mime::HTML)           => true,
            (mime::APPLICATION, mime::JAVASCRIPT)     => true,
            (mime::APPLICATION, mime::JSON)           => true,
            (mime::APPLICATION, mime::XML)            => true,
            (mime::APPLICATION, st) if
                st == "atom" ||
                st == "rss" ||
                st == "x-font-opentype" ||
                st == "x-font-truetype" ||
                st == "x-font-ttf" ||
                st == "xhtml" ||
                st == "xml"                          => true,
            (mime::IMAGE, mime::SVG)                 => true,
            (mime::FONT, st) if
                st == "opentype" ||
                st == "otf" ||
                st == "ttf"                          => true,
            _                                        => false
        }
        Err(e) => {
            debug!("not compressible: unable to parse content-type: {}: {:?}",
                  e, ctype_str);
            false
        }
    }
}

// Compute length of a HeaderMap serialized as a block of bytes. This includes
// the delimiter between name and value (": ") and CRLF after each line, but
// not any block padding.
fn len_of_headers(headers: &http::HeaderMap) -> usize {
    let mut size = 0;
    for (key, value) in headers.iter() {
        let kb: &[u8] = key.as_ref();
        size += kb.len();
        size += value.len();
        size += 4;
    }
    size
}

/// Wrapper holding a potentially encoding `Write` reference for the
/// underlying BARC `File` reference.
pub struct EncodeWrapper<'a>(Encoder<'a>);

enum Encoder<'a> {
    Plain(&'a File),
    Gzip(Box<GzEncoder<&'a File>>),
    #[cfg(feature = "brotli")]
    Brotli(Box<brotli::CompressorWriter<&'a File>>)
}

impl<'a> EncodeWrapper<'a> {

    /// Return wrapper for `Plain` (no compression) output.
    pub fn plain(file: &'a File) -> EncodeWrapper<'a> {
        EncodeWrapper(Encoder::Plain(file))
    }

    /// Return wrapper for `Gzip` output.
    pub fn gzip(file: &'a File, compression_level: u32)
        -> EncodeWrapper<'a>
    {
        EncodeWrapper(Encoder::Gzip(Box::new(
            GzEncoder::new(
                file,
                GzCompression::new(compression_level))
        )))
    }

    /// Return wrapper for `Brotli` output.
    #[cfg(feature = "brotli")]
    pub fn brotli(file: &'a File, compression_level: u32)
        -> EncodeWrapper<'a>
    {
        EncodeWrapper(Encoder::Brotli(Box::new(
            brotli::CompressorWriter::new(
                file,
                4096, //FIXME: tune?
                compression_level,
                21)
        )))
    }

    /// Return the `Compression` flag variant in use.
    pub fn mode(&self) -> Compression {
        match self.0 {
            Encoder::Plain(_) => Compression::Plain,
            Encoder::Gzip(_) => Compression::Gzip,
            #[cfg(feature = "brotli")]
            Encoder::Brotli(_) => Compression::Brotli,
        }
    }

    /// Consume the wrapper, finishing any encoding and flushing the
    /// completed write.
    pub fn finish(self) -> Result<(), BarcError> {
        match self.0 {
            Encoder::Plain(mut f) => {
                f.flush()?;
            }
            Encoder::Gzip(gze) => {
                gze.finish()?.flush()?;
            }
            #[cfg(feature = "brotli")]
            Encoder::Brotli(mut bcw) => {
                bcw.flush()?;
            }
        }
        Ok(())
    }
}

impl<'a> Write for EncodeWrapper<'a> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        match self.0 {
            Encoder::Plain(ref mut w) => w.write(buf),
            Encoder::Gzip(ref mut gze) => gze.write(buf),
            #[cfg(feature = "brotli")]
            Encoder::Brotli(ref mut bcw) => bcw.write(buf),
        }
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        match self.0 {
            Encoder::Plain(ref mut f) => f.flush(),
            Encoder::Gzip(ref mut gze) => gze.flush(),
            #[cfg(feature = "brotli")]
            Encoder::Brotli(ref mut bcw) => bcw.flush(),
        }
    }
}

/// Wrapper holding a decoder (providing `Read`) on a `Take` limited
/// to a record of a BARC file.
pub(crate) enum DecodeWrapper {
    Gzip(Box<GzDecoder<ReadSlice>>),
    #[cfg(feature = "brotli")]
    Brotli(Box<brotli::Decompressor<ReadSlice>>),
}

impl DecodeWrapper {
    pub(crate) fn new(comp: Compression, r: ReadSlice, _buf_size: usize)
        -> Result<DecodeWrapper, BarcError>
    {
        match comp {
            Compression::Gzip => {
                Ok(DecodeWrapper::Gzip(Box::new(GzDecoder::new(r))))
            }
            #[cfg(feature = "brotli")]
            Compression::Brotli => {
                Ok(DecodeWrapper::Brotli(Box::new(
                    brotli::Decompressor::new(r, _buf_size)
                )))
            }
            _ => Err(BarcError::DecoderUnsupported(comp))
        }
    }
}

impl Read for DecodeWrapper {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match *self {
            DecodeWrapper::Gzip(ref mut gze) => gze.read(buf),
            #[cfg(feature = "brotli")]
            DecodeWrapper::Brotli(ref mut bcw) => bcw.read(buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logger::test_logger;

    #[test]
    fn test_compressible_types() {
        assert!(test_logger());
        assert!(is_compressible_type_str("text/html"));
        assert!(is_compressible_type_str("Text/html; charset=utf8"));
        assert!(is_compressible_type_str("image/sVg"));
        assert!(is_compressible_type_str("application/rss"));
        assert!(is_compressible_type_str("font/TTF")); //case insensitive

        // httparse originating HeaderValue's should not have
        // leading/trailing whitespace, but we trim just in case
        assert!(is_compressible_type_str("  text/html"));
    }

    #[test]
    fn test_not_compressible_types() {
        assert!(test_logger());
        assert!(!is_compressible_type_str("image/png"));

        // Mime isn't as lenient as we might like
        assert!(!is_compressible_type_str("text/ html"));

        // Mime parse failures, logged as warnings, but no panics.
        assert!(!is_compressible_type_str(" "));
        assert!(!is_compressible_type_str(""));
        assert!(!is_compressible_type_str(";"));
        assert!(!is_compressible_type_str("/"));
        assert!(!is_compressible_type_str("/;"));
    }
}
