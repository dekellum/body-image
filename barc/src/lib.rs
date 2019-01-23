//! **B**ody **Arc**hive container file format, reader and writer.
//!
//! BARC is a container file format for the storage or one to many HTTP
//! request/response dialog records. A fixed length, ASCII-only record head
//! specifies lengths of a subsequent series of request and response header
//! blocks and bodies which are stored as raw (unencoded) bytes. When not
//! using the internal compression feature, the format is easily human
//! readable.  With compression, the `barc` CLI tool (*barc-cli* crate) can be
//! used to view records.
//!
//! See some sample files in source sample/*.barc.
//!
//! ## Other features:
//!
//! * An additional *meta*-headers block provides more recording details
//!   and can also be used to store application-specific values.
//!
//! * Sequential or random-access reads by record offset (which could be
//!   stored in an external index or database).
//!
//! * Single-writer sessions are guaranteed safe with N concurrent readers
//!   (in or out of process).
//!
//! * Optional per-record gzip or Brotli compression (headers and bodies)

#![deny(dead_code, unused_imports)]
#![warn(rust_2018_idioms)]

use std::cmp;
use std::fs::{File, OpenOptions};
use std::fmt;
use std::io;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::ops::{AddAssign,ShlAssign};
use std::sync::{Arc, Mutex, MutexGuard};
use std::path::Path;

use bytes::{BytesMut, BufMut};
use failure::{err_msg, Fail, Error as Flare};
use flate2::Compression as GzCompression;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use http;
use httparse;
use http::header::{HeaderName, HeaderValue};
use log::{debug, warn};
use olio::fs::rc::{ReadPos, ReadSlice};

#[cfg(feature = "brotli")]
use brotli;

use body_image::{
    BodyError, BodyImage, BodySink, Dialog, Encoding,
    Epilog, Prolog, Recorded, RequestRecorded, Tunables
};

mod try_conv;
pub use crate::try_conv::{TryFrom, TryInto};

/// Fixed record head size including CRLF terminator:
/// 54 Bytes
pub const V2_HEAD_SIZE: usize = 54;

/// Maximum total record length, excluding the record head:
/// 2<sup>48</sup> (256 TiB) - 1.
/// Note: this exceeds the file or partition size limits of many
/// file-systems.
pub const V2_MAX_RECORD: u64 = 0xfff_fff_fff_fff;

/// Maximum header (meta, request, response) block size, including
/// any CRLF terminator:
/// 2<sup>20</sup> (1 MiB) - 1.
pub const V2_MAX_HBLOCK: usize =        0xff_fff;

/// Maximum request body size, including any CRLF terminator:
/// 2<sup>40</sup> (1 TiB) - 1.
pub const V2_MAX_REQ_BODY: u64 = 0xf_fff_fff_fff;

/// Meta `HeaderName` for the complete URL used in the request.
#[inline]
pub fn hname_meta_url() -> http::header::HeaderName {
    static NAME: &str = "url";
    HeaderName::from_static(NAME)
}

/// Meta `HeaderName` for the HTTP method used in the request, e.g. "GET",
/// "POST", etc.
#[inline]
pub fn hname_meta_method() -> http::header::HeaderName {
    static NAME: &str = "method";
    HeaderName::from_static(NAME)
}

/// Meta `HeaderName` for the response version, e.g. "HTTP/1.1", "HTTP/2.0",
/// etc.
#[inline]
pub fn hname_meta_res_version() -> http::header::HeaderName {
    static NAME: &str = "response-version";
    HeaderName::from_static(NAME)
}

/// Meta `HeaderName` for the response numeric status code, SPACE, and then a
/// standardized _reason phrase_, e.g. "200 OK". The later is intended only
/// for human readers.
#[inline]
pub fn hname_meta_res_status() -> http::header::HeaderName {
    static NAME: &str = "response-status";
    HeaderName::from_static(NAME)
}

/// Meta `HeaderName` for a list of content or transfer encodings decoded for
/// the current response body. The value is in HTTP content-encoding header
/// format, e.g. "chunked, gzip".
#[inline]
pub fn hname_meta_res_decoded() -> http::header::HeaderName {
    static NAME: &str = "response-decoded";
    HeaderName::from_static(NAME)
}

/// Reference to a BARC File by `Path`, supporting up to 1 writer and N
/// readers concurrently.
pub struct BarcFile {
    path: Box<Path>,
    write_lock: Mutex<Option<File>>,
}

/// BARC file handle for write access.
pub struct BarcWriter<'a> {
    guard: MutexGuard<'a, Option<File>>
}

/// BARC file handle for read access. Each reader has its own file handle
/// and position.
pub struct BarcReader {
    file: ReadPos,
}

/// Error enumeration for all barc module errors.  This may be extended in the
/// future, so exhaustive matching is gently discouraged with an unused
/// variant.
#[derive(Debug)]
pub enum BarcError {
    /// Error with `BodySink` or `BodyImage`.
    Body(BodyError),

    /// IO errors, reading from or writing to a BARC file.
    Io(io::Error),

    /// Unknown `RecordType` byte flag.
    UnknownRecordType(u8),

    /// Unknown `Compression` byte flag.
    UnknownCompression(u8),

    /// Decoder unsupported for the `Compression` encoding found on read.
    DecoderUnsupported(Compression),

    /// Read an incomplete record head.
    ReadIncompleteRecHead(usize),

    /// Read an invalid record head.
    ReadInvalidRecHead,

    /// Read an invalid record head hex digit.
    ReadInvalidRecHeadHex(u8),

    /// Error parsing header name, value or block (with cause)
    InvalidHeader(Flare),

    /// Unused variant to both enable non-exhaustive matching and warn against
    /// exhaustive matching.
    _FutureProof,
}

impl fmt::Display for BarcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            BarcError::Body(ref be) =>
                write!(f, "With Body; {}", be),
            BarcError::Io(ref e) =>
                write!(f, "{}", e),
            BarcError::UnknownRecordType(b) =>
                write!(f, "Unknown record type flag [{}]", b),
            BarcError::UnknownCompression(b) =>
                write!(f, "Unknown compression flag [{}]", b),
            BarcError::DecoderUnsupported(c) =>
                write!(f, "No decoder for {:?}. Enable the feature?", c),
            BarcError::ReadIncompleteRecHead(l) =>
                write!(f, "Incomplete record head, len {}", l),
            BarcError::ReadInvalidRecHead =>
                write!(f, "Invalid record head suffix"),
            BarcError::ReadInvalidRecHeadHex(b) =>
                write!(f, "Invalid record head hex digit [{}]", b),
            BarcError::InvalidHeader(ref flare) =>
                write!(f, "Invalid header; {}", flare),
            BarcError::_FutureProof => unreachable!()
        }
    }
}

// Fail is implemented manually because we currently can't specify
// InvalidHeader's Flare type as #[cause], see:
// https://github.com/rust-lang-nursery/failure/issues/176
impl Fail for BarcError {
    fn cause(&self) -> Option<&dyn Fail> {
        match *self {
            BarcError::Body(ref be)               => Some(be),
            BarcError::Io(ref e)                  => Some(e),
            BarcError::InvalidHeader(ref flr)     => Some(flr.as_fail()),
            _ => None
        }
    }
}

impl From<io::Error> for BarcError {
    fn from(err: io::Error) -> BarcError {
        BarcError::Io(err)
    }
}

impl From<BodyError> for BarcError {
    fn from(err: BodyError) -> BarcError {
        BarcError::Body(err)
    }
}

/// A parsed record head.
#[derive(Debug)]
struct RecordHead {
    len:              u64,
    rec_type:         RecordType,
    compress:         Compression,
    meta:             usize,
    req_h:            usize,
    req_b:            u64,
    res_h:            usize,
}

/// An owned BARC record with public fields.
///
/// Additonal getter methods are found in trait implementations
/// [`RequestRecorded`](#impl-RequestRecorded), [`Recorded`](#impl-Recorded),
/// and [`MetaRecorded`](#impl-MetaRecorded).
#[derive(Clone, Debug, Default)]
pub struct Record {
    /// Record type.
    pub rec_type:         RecordType,

    /// Map of _meta_-headers for values which are not strictly part of the
    /// HTTP request or response headers. This can be extended with
    /// application specific name/value pairs.
    pub meta:             http::HeaderMap,

    /// Map of HTTP request headers.
    pub req_headers:      http::HeaderMap,

    /// Request body which may or may not be RAM resident.
    pub req_body:         BodyImage,

    /// Map of HTTP response headers.
    pub res_headers:      http::HeaderMap,

    /// Response body which may or may not be RAM resident.
    pub res_body:         BodyImage,
}

/// Access to BARC `Record` compatible objects by reference, extending
/// `Recorded` with meta-headers and a record type.
pub trait MetaRecorded: Recorded {
    /// Record type.
    fn rec_type(&self)    -> RecordType;

    /// Map of _meta_-headers for values which are not strictly part of the
    /// HTTP request or response headers.
    fn meta(&self)        -> &http::HeaderMap;
}

impl RequestRecorded for Record {
    fn req_headers(&self) -> &http::HeaderMap  { &self.req_headers }
    fn req_body(&self)    -> &BodyImage        { &self.req_body }
}

impl Recorded for Record {
    fn res_headers(&self) -> &http::HeaderMap  { &self.res_headers }
    fn res_body(&self)    -> &BodyImage        { &self.res_body }
}

impl MetaRecorded for Record {
    fn rec_type(&self)    -> RecordType        { self.rec_type }
    fn meta(&self)        -> &http::HeaderMap  { &self.meta }
}

impl TryFrom<Dialog> for Record {
    type Err = BarcError;

    /// Attempt to convert `Dialog` to `Record`.  This derives meta headers
    /// from various `Dialog` fields, and could potentially fail, based on
    /// header value constraints, with `BarcError::InvalidHeader`. Converting
    /// `Dialog::url` to the meta *url* header has the most potential, given
    /// `http::Uri` validation complexity, but any conversion failure would
    /// suggest an *http* crate bug or breaking change—as currently stated,
    /// allowed `Uri` bytes are a subset of allowed `HeaderValue` bytes.
    fn try_from(dialog: Dialog) -> Result<Self, Self::Err> {

        let (prolog, epilog) = dialog.explode();
        let mut meta = http::HeaderMap::with_capacity(6);
        let efn = &|e| BarcError::InvalidHeader(Flare::from(e));

        meta.append(
            hname_meta_url(),
            prolog.url.to_string().parse().map_err(efn)?
        );
        meta.append(
            hname_meta_method(),
            prolog.method.to_string().parse().map_err(efn)?
        );

        // FIXME: This relies on the debug format of version, e.g. "HTTP/1.1"
        // which might not be stable, but http::Version doesn't offer an enum
        // to match on, only constants.
        let v = format!("{:?}", epilog.version);
        meta.append(
            hname_meta_res_version(),
            v.parse().map_err(efn)?
        );

        meta.append(
            hname_meta_res_status(),
            epilog.status.to_string().parse().map_err(efn)?
        );

        if !epilog.res_decoded.is_empty() {
            let mut joined = String::with_capacity(20);
            for e in epilog.res_decoded {
                if !joined.is_empty() { joined.push_str(", "); }
                joined.push_str(&e.to_string());
            }
            meta.append(
                hname_meta_res_decoded(),
                joined.parse().map_err(efn)?
            );
        }
        Ok(Record {
            rec_type: RecordType::Dialog,
            meta,
            req_headers: prolog.req_headers,
            req_body:    prolog.req_body,
            res_headers: epilog.res_headers,
            res_body:    epilog.res_body,
        })
    }
}

/// Error enumeration for failures when converting from a `Record` to a
/// `Dialog`. This may be extended in the future, so exhaustive matching is
/// gently discouraged with an unused variant.
#[derive(Debug)]
pub enum DialogConvertError {
    /// No url meta header found.
    NoMetaUrl,

    /// The url meta header failed to parse as an `http::Uri`.
    InvalidUrl(http::uri::InvalidUriBytes),

    /// No method meta header found.
    NoMetaMethod,

    /// The method meta header failed to parse as an `http::Method`.
    InvalidMethod(http::method::InvalidMethod),

    /// No response-version meta header found.
    NoMetaResVersion,

    /// The response-version meta header did not match a known value.
    InvalidVersion(Vec<u8>),

    /// No response-status meta header found.
    NoMetaResStatus,

    /// The response-status meta header is not in a recognized format.
    MalformedMetaResStatus,

    /// The response-status meta header failed to be parsed as an
    /// `http::StatusCode`.
    InvalidStatusCode(http::status::InvalidStatusCode),

    /// The response-decoded meta header failed to be parsed.
    InvalidResDecoded(String),

    /// Unused variant to both enable non-exhaustive matching and warn against
    /// exhaustive matching.
    _FutureProof
}

impl fmt::Display for DialogConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DialogConvertError::NoMetaUrl =>
                write!(f, "No url meta header found"),
            DialogConvertError::InvalidUrl(ref iub) =>
                write!(f, "Invalid URI: {}", iub),
            DialogConvertError::NoMetaMethod =>
                write!(f, "No method meta header found"),
            DialogConvertError::InvalidMethod(ref im) =>
                write!(f, "Invalid HTTP Method: {}", im),
            DialogConvertError::NoMetaResVersion =>
                write!(f, "No response-version meta header found"),
            DialogConvertError::InvalidVersion(ref bs) => {
                if let Ok(s) = String::from_utf8(bs.clone()) {
                    write!(f, "Invalid HTTP Version: {}", s)
                } else {
                    write!(f, "Invalid HTTP Version: {:x?}", bs)
                }
            }
            DialogConvertError::NoMetaResStatus =>
                write!(f, "No response-status meta header found"),
            DialogConvertError::MalformedMetaResStatus =>
                write!(f, "The response-status meta header is malformed"),
            DialogConvertError::InvalidStatusCode(ref isc) =>
                write!(f, "Invalid HTTP status code: {}", isc),
            DialogConvertError::InvalidResDecoded(ref d) =>
                write!(f, "Invalid response-decoded header value: {}", d),
            DialogConvertError::_FutureProof => unreachable!()
        }
    }
}

impl Fail for DialogConvertError {
    fn cause(&self) -> Option<&dyn Fail> {
        match *self {
            DialogConvertError::InvalidUrl(ref iub)           => Some(iub),
            DialogConvertError::InvalidMethod(ref im)         => Some(im),
            DialogConvertError::InvalidStatusCode(ref isc)    => Some(isc),
            _ => None
        }
    }
}

impl TryFrom<Record> for Dialog {
    type Err = DialogConvertError;

    /// Attempt to convert `Record` to `Dialog`. This parses various meta
    /// header values to produce `Dialog` equivalents such as
    /// `http::StatusCode` and `http::Method`, which could fail, if the
    /// `Record` was not originally produced from a `Dialog` or was otherwise
    /// modified in an unsupported way.
    fn try_from(rec: Record) -> Result<Self, Self::Err> {
        let url = if let Some(uv) = rec.meta.get(hname_meta_url()) {
            http::Uri::from_shared(uv.as_bytes().into())
                .map_err(DialogConvertError::InvalidUrl)
        } else {
            Err(DialogConvertError::NoMetaUrl)
        }?;

        let method = if let Some(v) = rec.meta.get(hname_meta_method()) {
            http::Method::from_bytes(v.as_bytes())
                .map_err(DialogConvertError::InvalidMethod)
        } else {
            Err(DialogConvertError::NoMetaMethod)
        }?;

        let version = if let Some(v) = rec.meta.get(hname_meta_res_version()) {
            let vb = v.as_bytes();
            match vb {
                b"HTTP/0.9" => http::Version::HTTP_09,
                b"HTTP/1.0" => http::Version::HTTP_10,
                b"HTTP/1.1" => http::Version::HTTP_11,
                b"HTTP/2.0" => http::Version::HTTP_2,
                _ => {
                    return Err(DialogConvertError::InvalidVersion(vb.to_vec()));
                }
            }
        } else {
            return Err(DialogConvertError::NoMetaResVersion);
        };

        let status = if let Some(v) = rec.meta.get(hname_meta_res_status()) {
            let vbs = v.as_bytes();
            if vbs.len() >= 3 {
                http::StatusCode::from_bytes(&vbs[0..3])
                    .map_err(DialogConvertError::InvalidStatusCode)
            } else {
                Err(DialogConvertError::MalformedMetaResStatus)
            }
        } else {
            Err(DialogConvertError::NoMetaResStatus)
        }?;

        let res_decoded = if let Some(v) = rec.meta.get(hname_meta_res_decoded()) {
            if let Ok(dcds) = v.to_str() {
                let mut encodes = Vec::with_capacity(4);
                for enc in dcds.split(',') {
                    let enc = enc.trim();
                    encodes.push(match enc {
                        "chunked" => Encoding::Chunked,
                        "deflate" => Encoding::Deflate,
                        "gzip"    => Encoding::Gzip,
                        "br"      => Encoding::Brotli,
                        _ => {
                            return Err(DialogConvertError::InvalidResDecoded(
                                enc.to_string()
                            ));
                        }
                    })
                }
                encodes
            } else {
                return Err(DialogConvertError::InvalidResDecoded(
                    format!("{:x?}", v.as_bytes())
                ));
            }
        } else {
            Vec::with_capacity(0)
        };

        Ok(Dialog::new(
            Prolog {
                method,
                url,
                req_headers: rec.req_headers,
                req_body:    rec.req_body
            },
            Epilog {
                version,
                status,
                res_decoded,
                res_headers: rec.res_headers,
                res_body:    rec.res_body
            }
        ))
    }
}

/// BARC record type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecordType {
    /// Used internally to _reserve_ a BARC record head.
    Reserved,

    /// A complete HTTP request and response dialog between a client
    /// and server. See `Dialog`.
    Dialog,
}

impl RecordType {
    /// Return (char) flag for variant.
    fn flag(self) -> char {
        match self {
            RecordType::Reserved => 'R',
            RecordType::Dialog => 'D',
        }
    }

    /// Return variant for (byte) flag, or fail.
    fn from_byte(f: u8) -> Result<Self, BarcError> {
        match f {
            b'R' => Ok(RecordType::Reserved),
            b'D' => Ok(RecordType::Dialog),
            _ => Err(BarcError::UnknownRecordType(f))
        }
    }
}

impl Default for RecordType {
    /// Defaults to `Dialog`.
    fn default() -> RecordType { RecordType::Dialog }
}

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
    fn flag(self) -> char {
        match self {
            Compression::Plain   => 'P',
            Compression::Gzip    => 'Z',
            Compression::Brotli  => 'B',
        }
    }

    /// Return variant for (byte) flag, or fail.
    fn from_byte(f: u8) -> Result<Self, BarcError> {
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
        Ok(EncodeWrapper::Plain(file))
    }
}

/// Strategy for gzip compression. Will not compress if a minimum
/// length estimate is not reached.
#[derive(Clone, Copy, Debug)]
pub struct GzipCompressStrategy {
    min_len: u64,
    compression_level: u32,
}

impl GzipCompressStrategy {
    /// Set minimum length in bytes for when to use compression.
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
}

impl Default for GzipCompressStrategy {
    fn default() -> Self {
        Self { min_len: 4 * 1024,
               compression_level: 6 }
    }
}

impl CompressStrategy for GzipCompressStrategy {
    fn wrap_encoder<'a>(&self, rec: &dyn MetaRecorded, file: &'a File)
        -> Result<EncodeWrapper<'a>, BarcError>
    {
        // FIXME: This only considers req/res body lengths
        let est_len = rec.req_body().len() + rec.res_body().len();
        if est_len >= self.min_len {
            Ok(EncodeWrapper::Gzip(Box::new(
                GzEncoder::new(file, GzCompression::new(self.compression_level))
            )))
        } else {
            Ok(EncodeWrapper::Plain(file))
        }
    }
}

/// Strategy for Brotli compression. Will not compress if a minimum
/// length estimate is not reached.
#[cfg(feature = "brotli")]
#[derive(Clone, Copy, Debug)]
pub struct BrotliCompressStrategy {
    min_len: u64,
    compression_level: u32,
}

#[cfg(feature = "brotli")]
impl BrotliCompressStrategy {
    /// Set minimum length in bytes for when to use compression.
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
}

#[cfg(feature = "brotli")]
impl Default for BrotliCompressStrategy {
    fn default() -> Self {
        Self { min_len: 1024,
               compression_level: 6 }
    }
}

#[cfg(feature = "brotli")]
impl CompressStrategy for BrotliCompressStrategy {
    fn wrap_encoder<'a>(&self, rec: &dyn MetaRecorded, file: &'a File)
        -> Result<EncodeWrapper<'a>, BarcError>
    {
        // FIXME: This only considers req/res body lengths
        let est_len = rec.req_body().len() + rec.res_body().len();
        if est_len >= self.min_len {
            Ok(EncodeWrapper::Brotli(Box::new(
                brotli::CompressorWriter::new(
                    file,
                    4096, //FIXME: tune?
                    self.compression_level,
                    21)
            )))
        } else {
            Ok(EncodeWrapper::Plain(file))
        }
    }
}

/// Wrapper holding a potentially encoding `Write` reference for the
/// underlying BARC `File` reference.
pub enum EncodeWrapper<'a> {
    Plain(&'a File),
    Gzip(Box<GzEncoder<&'a File>>),
    #[cfg(feature = "brotli")]
    Brotli(Box<brotli::CompressorWriter<&'a File>>)
}

impl<'a> EncodeWrapper<'a> {
    /// Return the `Compression` flag variant in use.
    pub fn mode(&self) -> Compression {
        match *self {
            EncodeWrapper::Plain(_) => Compression::Plain,
            EncodeWrapper::Gzip(_) => Compression::Gzip,
            #[cfg(feature = "brotli")]
            EncodeWrapper::Brotli(_) => Compression::Brotli,
        }
    }

    /// Return a `Write` reference for self.
    pub fn as_write(&mut self) -> &mut dyn Write {
        match *self {
            EncodeWrapper::Plain(ref mut f) => f,
            EncodeWrapper::Gzip(ref mut gze) => gze,
            #[cfg(feature = "brotli")]
            EncodeWrapper::Brotli(ref mut bcw) => bcw,
        }
    }

    /// Consume the wrapper, finishing any encoding and flushing the
    /// completed write.
    pub fn finish(self) -> Result<(), BarcError> {
        match self {
            EncodeWrapper::Plain(mut f) => {
                f.flush()?;
            }
            EncodeWrapper::Gzip(gze) => {
                gze.finish()?.flush()?;
            }
            #[cfg(feature = "brotli")]
            EncodeWrapper::Brotli(mut bcw) => {
                bcw.flush()?;
            }
        }
        Ok(())
    }
}

const CRLF: &[u8] = b"\r\n";

const WITH_CRLF: bool = true;
const NO_CRLF:   bool = false;

const V2_RESERVE_HEAD: RecordHead = RecordHead {
    len: 0,
    rec_type: RecordType::Reserved,
    compress: Compression::Plain,
    meta: 0,
    req_h: 0,
    req_b: 0,
    res_h: 0
};

impl BarcFile {
    /// Return new instance for the specified path, which may be an
    /// existing file, or one to be created when `writer` is opened.
    pub fn new<P>(path: P) -> BarcFile
        where P: AsRef<Path>
    {
        // Save off owned path to re-open for readers
        let path: Box<Path> = path.as_ref().into();
        let write_lock = Mutex::new(None);
        BarcFile { path, write_lock }
    }

    /// Get a writer for this file, opening the file for write (and
    /// possibly creating it, or erroring) if this is the first time
    /// called. May block on the write lock, as only one `BarcWriter`
    /// instance is allowed.
    pub fn writer(&self) -> Result<BarcWriter<'_>, BarcError> {
        let mut guard = self.write_lock.lock().unwrap(); // FIXME:
        // PoisonError is not send, so can't map to BarcError

        if (*guard).is_none() {
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&self.path)?;
            // FIXME: Use fs2 crate for: file.try_lock_exclusive()?
            *guard = Some(file);
        }

        Ok(BarcWriter { guard })
    }

    /// Get a reader for this file. Errors if the file does not exist.
    pub fn reader(&self) -> Result<BarcReader, BarcError> {
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)?;
        // FIXME: Use fs2 crate for: file.try_lock_shared()?
        Ok(BarcReader { file: ReadPos::new(Arc::new(file), 0) })
    }
}

impl<'a> BarcWriter<'a> {
    /// Write a new record, returning the record's offset from the
    /// start of the BARC file. The writer position is then advanced
    /// to the end of the file, for the next `write`.
    pub fn write(
        &mut self,
        rec: &dyn MetaRecorded,
        strategy: &dyn CompressStrategy)
        -> Result<u64, BarcError>
    {
        // BarcFile::writer() guarantees Some(File)
        let file = &mut *self.guard.as_mut().unwrap();

        // Write initial head as reserved place holder
        let start = file.seek(SeekFrom::End(0))?;
        write_record_head(file, &V2_RESERVE_HEAD)?;
        file.flush()?;

        // Write the record per strategy
        let mut head = write_record(file, rec, strategy)?;

        // Use new file offset to indicate total length
        let end = file.seek(SeekFrom::Current(0))?;
        let orig_len = head.len;
        assert!(end >= (start + (V2_HEAD_SIZE as u64)));
        head.len = end - start - (V2_HEAD_SIZE as u64);
        if head.compress == Compression::Plain {
            assert_eq!(orig_len, head.len);
        } else if orig_len < head.len {
            warn!("Compression *increased* record size from \
                   {} to {} bytes",
                  orig_len, head.len);
        }

        // Seek back and write final record head, with known size
        file.seek(SeekFrom::Start(start))?;
        write_record_head(file, &head)?;

        // Seek to end and flush
        file.seek(SeekFrom::End(0))?;
        file.flush()?;

        Ok(start)
    }
}

// Write the record, returning a preliminary `RecordHead` with
// observed (not compressed) lengths.
fn write_record(
    file: &mut File,
    rec: &dyn MetaRecorded,
    strategy: &dyn CompressStrategy)
    -> Result<RecordHead, BarcError>
{
    let mut wrapper = strategy.wrap_encoder(rec, file)?;
    let compress = wrapper.mode();
    let with_crlf = compress == Compression::Plain;
    let head = {
        let fout = wrapper.as_write();

        let meta = write_headers(fout, with_crlf, rec.meta())?;

        let req_h = write_headers(fout, with_crlf, rec.req_headers())?;
        let req_b = write_body(fout, with_crlf, rec.req_body())?;

        let res_h = write_headers(fout, with_crlf, rec.res_headers())?;

        // Compute total thus far, excluding the fixed head length
        let mut len: u64 = (meta + req_h + res_h) as u64 + req_b;

        assert!((len + rec.res_body().len() + 2) <= V2_MAX_RECORD,
                "body exceeds size limit");

        let res_b = write_body(fout, with_crlf, rec.res_body())?;
        len += res_b;

        RecordHead {
            len, // uncompressed length
            rec_type: rec.rec_type(),
            compress,
            meta,
            req_h,
            req_b,
            res_h }
    };

    wrapper.finish()?;
    Ok(head)
}

// Write record head to out, asserting the various length constraints.
fn write_record_head(out: &mut dyn Write, head: &RecordHead)
    -> Result<(), BarcError>
{
    // Check input ranges
    assert!(head.len   <= V2_MAX_RECORD,   "len exceeded");
    assert!(head.meta  <= V2_MAX_HBLOCK,   "meta exceeded");
    assert!(head.req_h <= V2_MAX_HBLOCK,   "req_h exceeded");
    assert!(head.req_b <= V2_MAX_REQ_BODY, "req_b exceeded");
    assert!(head.res_h <= V2_MAX_HBLOCK,   "res_h exceeded");

    let size = write_all_len(out, format!(
        // ---6------19---22-----28-----34------45----50------54
        "BARC2 {:012x} {}{} {:05x} {:05x} {:010x} {:05x}\r\n\r\n",
        head.len, head.rec_type.flag(), head.compress.flag(),
        head.meta, head.req_h, head.req_b, head.res_h
    ).as_bytes())?;
    assert_eq!(size, V2_HEAD_SIZE, "wrong record head size");
    Ok(())
}

/// Write header block to out, with optional CR+LF end padding, and return the
/// length written. This is primarily an implementation detail of `BarcWriter`,
/// but is made public for its general diagnostic utility.
pub fn write_headers(
    out: &mut dyn Write,
    with_crlf: bool,
    headers: &http::HeaderMap)
    -> Result<usize, BarcError>
{
    let mut size = 0;
    for (key, value) in headers.iter() {
        size += write_all_len(out, key.as_ref())?;
        size += write_all_len(out, b": ")?;
        size += write_all_len(out, value.as_bytes())?;
        size += write_all_len(out, CRLF)?;
    }
    if with_crlf && size > 0 {
        size += write_all_len(out, CRLF)?;
    }
    assert!(size <= V2_MAX_HBLOCK);
    Ok(size)
}

/// Write body to out, with optional CR+LF end padding, and return the length
/// written.  This is primarily an implementation detail of `BarcWriter`, but
/// is made public for its general diagnostic utility.
pub fn write_body(out: &mut dyn Write, with_crlf: bool, body: &BodyImage)
    -> Result<u64, BarcError>
{
    let mut size = body.write_to(out)?;
    if with_crlf && size > 0 {
        size += write_all_len(out, CRLF)? as u64;
    }
    Ok(size)
}

// Like `write_all`, but return the length of the provided byte slice.
fn write_all_len(out: &mut dyn Write, bs: &[u8]) -> Result<usize, BarcError> {
    out.write_all(bs)?;
    Ok(bs.len())
}

impl BarcReader {

    /// Read and return the next Record or None if EOF. The provided Tunables
    /// `max_body_ram` controls, depending on record sizes and compression,
    /// whether the request and response bodies are read directly into RAM,
    /// buffered in a file, or deferred via a `ReadSlice`.
    pub fn read(&mut self, tune: &Tunables)
        -> Result<Option<Record>, BarcError>
    {
        let fin = &mut self.file;
        let start = fin.tell();

        let rhead = match read_record_head(fin) {
            Ok(Some(rh)) => rh,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e)
        };

        let rec_type = rhead.rec_type;

        // With a concurrent writer, its possible to see an incomplete,
        // Reserved record head, followed by an empty or partial record
        // payload.  In this case, seek back to start and return None.
        if rec_type == RecordType::Reserved {
            fin.seek(SeekFrom::Start(start))?;
            return Ok(None);
        }

        if rhead.compress != Compression::Plain {
            let end = fin.tell() + rhead.len;
            let rec = read_compressed(
                fin.subslice(fin.tell(), end), &rhead, tune
            )?;
            fin.seek(SeekFrom::Start(end))?;
            return Ok(Some(rec))
        }

        let meta = read_headers(fin, WITH_CRLF, rhead.meta)?;
        let req_headers = read_headers(fin, WITH_CRLF, rhead.req_h)?;

        let req_body = if rhead.req_b <= tune.max_body_ram() {
            read_body_ram(fin, WITH_CRLF, rhead.req_b as usize)
        } else {
            slice_body(fin, rhead.req_b)
        }?;
        let res_headers = read_headers(fin, WITH_CRLF, rhead.res_h)?;

        let body_len = rhead.len - (fin.tell() - start - (V2_HEAD_SIZE as u64));

        let res_body = if body_len <= tune.max_body_ram() {
            read_body_ram(fin, WITH_CRLF, body_len as usize)
        } else {
            slice_body(fin, body_len)
        }?;

        Ok(Some(Record { rec_type, meta, req_headers, req_body,
                         res_headers, res_body }))
    }

    /// Returns the current offset in bytes of this reader, which starts as 0
    /// and is advanced by each succesful return from `read` or updated via
    /// `seek`.
    pub fn offset(&self) -> u64 {
        self.file.tell()
    }

    /// Seek to a known byte offset (e.g. 0 or as returned from
    /// `BarcWriter::write` or `offset`) from the start of the BARC file. This
    /// effects subsequent calls to `read`, which may error if the position is
    /// not to a valid record head.
    pub fn seek(&mut self, offset: u64) -> Result<(), BarcError> {
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(())
    }
}

/// Wrapper holding a decoder (providing `Read`) on a `Take` limited
/// to a record of a BARC file.
enum DecodeWrapper {
    Gzip(Box<GzDecoder<ReadSlice>>),
    #[cfg(feature = "brotli")]
    Brotli(Box<brotli::Decompressor<ReadSlice>>),
}

impl DecodeWrapper {
    fn new(comp: Compression, r: ReadSlice, _buf_size: usize)
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

    /// Return a `Read` reference for self.
    fn as_read(&mut self) -> &mut dyn Read {
        match *self {
            DecodeWrapper::Gzip(ref mut gze) => gze,
            #[cfg(feature = "brotli")]
            DecodeWrapper::Brotli(ref mut bcw) => bcw,
        }
    }
}

// Read and return a compressed `Record`. This is specialized for
// NO_CRLF and since bodies can't be directly mapped from the file.
fn read_compressed(rslice: ReadSlice, rhead: &RecordHead, tune: &Tunables)
    -> Result<Record, BarcError>
{
    // Decoder over limited `ReadSlice` of compressed record len
    let mut wrapper = DecodeWrapper::new(
        rhead.compress,
        rslice,
        tune.buffer_size_ram())?;

    let fin = wrapper.as_read();

    let rec_type = rhead.rec_type;

    let meta = read_headers(fin, NO_CRLF, rhead.meta)?;

    let req_headers = read_headers(fin, NO_CRLF, rhead.req_h)?;

    let req_body = if rhead.req_b <= tune.max_body_ram() {
        read_body_ram(fin, NO_CRLF, rhead.req_b as usize)?
    } else {
        read_body_fs(fin, rhead.req_b, tune)?
    };

    let res_headers = read_headers(fin, NO_CRLF, rhead.res_h)?;

    // When compressed, we don't actually know the final size of the
    // response body, so start small and use read_to_body. This may
    // return `Ram` or `FsRead` states.
    let res_body = BodyImage::read_from(fin, 4096, tune)?;

    Ok(Record { rec_type, meta, req_headers, req_body, res_headers, res_body })
}

// Return RecordHead or None if EOF
fn read_record_head(r: &mut dyn Read)
    -> Result<Option<RecordHead>, BarcError>
{
    let mut buf = [0u8; V2_HEAD_SIZE];

    let size = read_record_head_buf(r, &mut buf)?;
    if size == 0 {
        return Ok(None);
    }
    if size != V2_HEAD_SIZE {
        return Err(BarcError::ReadIncompleteRecHead(size));
    }
    if &buf[0..6] != b"BARC2 " {
        return Err(BarcError::ReadInvalidRecHead);
    }

    let len       = parse_hex(&buf[6..18])?;
    let rec_type  = RecordType::from_byte(buf[19])?;
    let compress  = Compression::from_byte(buf[20])?;
    let meta      = parse_hex(&buf[22..27])?;
    let req_h     = parse_hex(&buf[28..33])?;
    let req_b     = parse_hex(&buf[34..44])?;
    let res_h     = parse_hex(&buf[45..50])?;
    Ok(Some(RecordHead { len, rec_type, compress, meta, req_h, req_b, res_h }))
}

// Like `Read::read_exact` but we need to distinguish 0 bytes read
// (EOF) from partial bytes read (a format error), so it also returns
// the number of bytes read.
fn read_record_head_buf(r: &mut dyn Read, mut buf: &mut [u8])
    -> Result<usize, BarcError>
{
    let mut size = 0;
    loop {
        match r.read(buf) {
            Ok(0) => break,
            Ok(n) => {
                size += n;
                if size >= V2_HEAD_SIZE {
                    break;
                }
                let t = buf;
                buf = &mut t[n..];
            }
            Err(e) => {
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                } else {
                    return Err(e.into());
                }
            }
        }
    }
    Ok(size)
}

// Read lowercase hexadecimal unsigned value directly from bytes.
fn parse_hex<T>(buf: &[u8]) -> Result<T, BarcError>
    where T: AddAssign<T> + From<u8> + ShlAssign<u8>
{
    let mut v = T::from(0u8);
    for d in buf {
        v <<= 4u8;
        if *d >= b'0' && *d <= b'9' {
            v += T::from(*d - b'0');
        } else if *d >= b'a' && *d <= b'f' {
            v += T::from(10 + (*d - b'a'));
        } else {
            return Err(BarcError::ReadInvalidRecHeadHex(*d));
        }
    }
    Ok(v)
}

// Reader header block of len bytes to HeaderMap.
fn read_headers(r: &mut dyn Read, with_crlf: bool, len: usize)
    -> Result<http::HeaderMap, BarcError>
{
    if len == 0 {
        return Ok(http::HeaderMap::with_capacity(0));
    }

    assert!(len > 2);

    let tlen = if with_crlf { len } else { len + 2 };
    let mut buf = BytesMut::with_capacity(tlen);
    unsafe {
        r.read_exact(&mut buf.bytes_mut()[..len])?;
        buf.advance_mut(len);
    }

    // Add CRLF for parsing if its not already present
    // (e.g. Compression::Plain padding)
    if !with_crlf {
        buf.put_slice(CRLF)
    }
    parse_headers(&buf[..])
}

// Parse header byte slice to HeaderMap.
fn parse_headers(buf: &[u8]) -> Result<http::HeaderMap, BarcError> {
    let mut headbuf = [httparse::EMPTY_HEADER; 128];

    // FIXME: httparse will return TooManyHeaders if headbuf isn't
    // large enough. Hyper 0.11.15 allocates 100, so 128 is room for
    // _even more_ (sigh). Might be better to just replace this with
    // our own parser, as the grammar isn't particularly complex.

    match httparse::parse_headers(buf, &mut headbuf) {
        Ok(httparse::Status::Complete((size, heads))) => {
            let mut hmap = http::HeaderMap::with_capacity(heads.len());
            assert_eq!(size, buf.len());
            for h in heads {
                let name = h.name.parse::<HeaderName>()
                    .map_err(|e| BarcError::InvalidHeader(Flare::from(e)))?;
                let value = HeaderValue::from_bytes(h.value)
                    .map_err(|e| BarcError::InvalidHeader(Flare::from(e)))?;
                hmap.append(name, value);
            }
            Ok(hmap)
        }
        Ok(httparse::Status::Partial) => {
            Err(BarcError::InvalidHeader(
                err_msg("Header block not CRLF terminated")
            ))
        }
        Err(e) => Err(BarcError::InvalidHeader(Flare::from(e)))
    }
}

// Read into `BodyImage` of state `Ram` as a single buffer.
fn read_body_ram(r: &mut dyn Read, with_crlf: bool, len: usize)
    -> Result<BodyImage, BarcError>
{
    if len == 0 {
        return Ok(BodyImage::empty());
    }

    assert!(!with_crlf || len > 2);

    let mut buf = BytesMut::with_capacity(len);
    unsafe {
        r.read_exact(&mut buf.bytes_mut()[..len])?;
        let l = if with_crlf { len - 2 } else { len };
        buf.advance_mut(l);
    }

    Ok(BodyImage::from_slice(buf.freeze()))
}

// Read into `BodyImage` state `FsRead`. Assumes no CRLF terminator
// (only used for compressed records).
fn read_body_fs(r: &mut dyn Read, len: u64, tune: &Tunables)
    -> Result<BodyImage, BarcError>
{
    if len == 0 {
        return Ok(BodyImage::empty());
    }

    let mut body = BodySink::with_fs(tune.temp_dir())?;
    let mut buf = BytesMut::with_capacity(tune.buffer_size_fs());
    loop {
        let rlen = {
            let b = unsafe { buf.bytes_mut() };
            let limit = cmp::min(b.len() as u64, len - body.len()) as usize;
            assert!(limit > 0);
            match r.read(&mut b[..limit]) {
                Ok(l) => l,
                Err(e) => {
                    if e.kind() == ErrorKind::Interrupted {
                        continue;
                    } else {
                        return Err(e.into());
                    }
                }
            }
        };
        if rlen == 0 {
            break;
        }
        unsafe { buf.advance_mut(rlen); }
        debug!("Write (Fs) buffer len {}", rlen);
        body.write_all(&buf)?;

        if body.len() < len {
            buf.clear();
        } else {
            assert_eq!(body.len(), len);
            break;
        }
    }
    let body = body.prepare()?;
    Ok(body)
}

// Return `BodyImage::FsReadSlice` for an uncompressed body in file, at the
// current offset of `ReadPos`, for the given length.
fn slice_body(rp: &mut ReadPos, len: u64) -> Result<BodyImage, BarcError> {
    assert!(len > 2);
    let offset = rp.tell();

    // Seek past the body, as if read.
    rp.seek(SeekFrom::Current(len as i64))?;

    let rslice = rp.subslice(offset, offset + len - 2); // - CRLF

    // Safety: There is only appending writes, so within reason, the slice
    // (and any later memory mapping) should be safe from concurrent
    // modification and UB. The `allow(unused_unsafe)` is because the method
    // is actually not flagged as `unsafe` when the *memmap* feature is
    // disabled.
    #[allow(unused_unsafe)]
    {
        Ok(unsafe { BodyImage::from_read_slice(rslice) })
    }
}

#[cfg(test)]
mod barc_tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use http::header::{AGE, REFERER, VIA};
    use super::*;
    use body_image::Tuner;
    use failure::Error as Flare;

    fn barc_test_file(name: &str) -> Result<PathBuf, Flare> {
        let target = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/../target/testmp", target);
        let tpath = Path::new(&path);
        fs::create_dir_all(tpath)?;

        let fname = tpath.join(name);
        if fname.exists() {
            fs::remove_file(&fname)?;
        }
        Ok(fname)
    }

    #[test]
    fn test_write_read_small() {
        let fname = barc_test_file("small.barc").unwrap();
        let strategy = NoCompressStrategy::default();
        write_read_small(&fname, &strategy).unwrap();
    }

    #[test]
    fn test_write_read_small_gzip() {
        let fname = barc_test_file("small_gzip.barc").unwrap();
        let strategy = GzipCompressStrategy::default().set_min_len(0);
        write_read_small(&fname, &strategy).unwrap();
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn test_write_read_small_brotli() {
        let fname = barc_test_file("small_brotli.barc").unwrap();
        let strategy = BrotliCompressStrategy::default().set_min_len(0);
        write_read_small(&fname, &strategy).unwrap();
    }

    fn write_read_small(fname: &PathBuf, strategy: &dyn CompressStrategy)
        -> Result<(), Flare>
    {
        let bfile = BarcFile::new(fname);

        let req_body_str = "REQUEST BODY";
        let res_body_str = "RESPONSE BODY";

        let rec_type = RecordType::Dialog;
        let mut meta = http::HeaderMap::new();
        meta.insert(AGE, "0".parse()?);

        let mut req_headers = http::HeaderMap::new();
        req_headers.insert(REFERER, "http:://other.com".parse()?);
        let req_body = BodyImage::from_slice(req_body_str);

        let mut res_headers = http::HeaderMap::new();
        res_headers.insert(VIA, "test".parse()?);
        let res_body = BodyImage::from_slice(res_body_str);

        let mut writer = bfile.writer()?;
        assert!(fname.exists()); // on writer creation
        writer.write(&Record { rec_type, meta,
                               req_headers, req_body,
                               res_headers, res_body },
                     strategy)?;

        let tune = Tunables::new();
        let mut reader = bfile.reader()?;
        let record = reader.read(&tune)?.unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 1);
        assert_eq!(record.req_headers.len(), 1);
        assert_eq!(record.req_body.len(), req_body_str.len() as u64);
        assert_eq!(record.res_headers.len(), 1);
        assert_eq!(record.res_body.len(), res_body_str.len() as u64);

        let record = reader.read(&tune)?;
        assert!(record.is_none());
        Ok(())
    }

    #[test]
    fn test_write_read_empty_record() {
        let fname = barc_test_file("empty_record.barc").unwrap();
        let strategy = NoCompressStrategy::default();
        write_read_empty_record(&fname, &strategy).unwrap();;
    }

    #[test]
    fn test_write_read_empty_record_gzip() {
        let fname = barc_test_file("empty_record_gzip.barc").unwrap();
        let strategy = GzipCompressStrategy::default().set_min_len(0);
        write_read_empty_record(&fname, &strategy).unwrap();
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn test_write_read_empty_record_brotli() {
        let fname = barc_test_file("empty_record_brotli.barc").unwrap();
        let strategy = BrotliCompressStrategy::default().set_min_len(0);
        write_read_empty_record(&fname, &strategy).unwrap();
    }

    fn write_read_empty_record(fname: &PathBuf, strategy: &dyn CompressStrategy)
        -> Result<(), Flare>
    {
        let bfile = BarcFile::new(fname);

        let mut writer = bfile.writer()?;

        writer.write(&Record::default(), strategy)?;

        let tune = Tunables::new();
        let mut reader = bfile.reader()?;
        let record = reader.read(&tune)?.unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 0);
        assert_eq!(record.req_headers.len(), 0);
        assert_eq!(record.req_body.len(), 0);
        assert_eq!(record.res_headers.len(), 0);
        assert_eq!(record.res_body.len(), 0);

        let record = reader.read(&tune)?;
        assert!(record.is_none());
        Ok(())
    }

    #[test]
    fn test_write_read_large() {
        let fname = barc_test_file("large.barc").unwrap();
        let strategy = NoCompressStrategy::default();
        write_read_large(&fname, &strategy).unwrap();;
    }

    #[test]
    fn test_write_read_large_gzip() {
        let fname = barc_test_file("large_gzip.barc").unwrap();
        let strategy = GzipCompressStrategy::default();
        write_read_large(&fname, &strategy).unwrap();
    }

    #[test]
    fn test_write_read_large_gzip_0() {
        let fname = barc_test_file("large_gzip_0.barc").unwrap();
        let strategy = GzipCompressStrategy::default().set_compression_level(0);
        write_read_large(&fname, &strategy).unwrap();
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn test_write_read_large_brotli() {
        let fname = barc_test_file("large_brotli.barc").unwrap();
        let strategy = BrotliCompressStrategy::default();
        write_read_large(&fname, &strategy).unwrap();
    }

    fn write_read_large(fname: &PathBuf, strategy: &dyn CompressStrategy)
        -> Result<(), Flare>
    {
        let bfile = BarcFile::new(fname);

        let mut writer = bfile.writer()?;

        let lorem_ipsum =
           "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
            sed do eiusmod tempor incididunt ut labore et dolore magna \
            aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
            ullamco laboris nisi ut aliquip ex ea commodo \
            consequat. Duis aute irure dolor in reprehenderit in \
            voluptate velit esse cillum dolore eu fugiat nulla \
            pariatur. Excepteur sint occaecat cupidatat non proident, \
            sunt in culpa qui officia deserunt mollit anim id est \
            laborum. ";

        let req_reps =   500;
        let res_reps = 1_000;

        let mut req_body = BodySink::with_ram_buffers(req_reps);
        for _ in 0..req_reps {
            req_body.save(lorem_ipsum)?;
        }
        let req_body = req_body.prepare()?;

        let mut res_body = BodySink::with_ram_buffers(res_reps);
        for _ in 0..res_reps {
            res_body.save(lorem_ipsum)?;
        }
        let res_body = res_body.prepare()?;

        writer.write(&Record { req_body, res_body, ..Record::default()},
                     strategy)?;

        let tune = Tunables::new();
        let mut reader = bfile.reader()?;
        let record = reader.read(&tune)?.unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 0);
        assert_eq!(record.req_headers.len(), 0);
        assert_eq!(record.req_body.len(),
                   (lorem_ipsum.len() * req_reps) as u64);
        assert_eq!(record.res_headers.len(), 0);
        assert_eq!(record.res_body.len(),
                   (lorem_ipsum.len() * res_reps) as u64);

        let record = reader.read(&tune)?;
        assert!(record.is_none());
        Ok(())
    }

    #[test]
    fn test_write_read_parallel() {
        let fname = barc_test_file("parallel.barc").unwrap();
        let bfile = BarcFile::new(&fname);
        // Temp writer to ensure file is created
        {
            let mut _writer = bfile.writer().unwrap();
        }

        let res_body_str = "RESPONSE BODY";

        // Establish reader.
        let tune = Tunables::new();
        let mut reader = bfile.reader().unwrap();
        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());

        // Write record with new writer
        let mut writer = bfile.writer().unwrap();
        let res_body = BodyImage::from_slice(res_body_str);

        let offset = writer.write(&Record {
            res_body, ..Record::default() },
            &NoCompressStrategy::default()).unwrap();
        assert_eq!(offset, 0);
        reader.seek(offset).unwrap();

        let record = reader.read(&tune).unwrap().unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 0);
        assert_eq!(record.req_headers.len(), 0);
        assert_eq!(record.req_body.len(), 0);
        assert_eq!(record.res_headers.len(), 0);
        assert_eq!(record.res_body.len(), res_body_str.len() as u64);

        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());

        // Write another, empty
        writer.write(&Record::default(),
                     &NoCompressStrategy::default()).unwrap();

        let record = reader.read(&tune).unwrap().unwrap();
        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.res_body.len(), 0);

        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());
    }

    #[test]
    fn test_read_sample() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/example.barc");
        let mut reader = bfile.reader().unwrap();
        let record = reader.read(&tune).unwrap().unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 5);
        assert_eq!(record.req_headers.len(), 4);
        assert!(record.req_body.is_empty());
        assert_eq!(record.res_headers.len(), 11);

        assert!(record.res_body.is_ram());
        let mut br = record.res_body.reader();
        let mut buf = Vec::with_capacity(2048);
        br.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.len(), 1270);
        assert_eq!(&buf[0..15], b"<!doctype html>");
        assert_eq!(&buf[(buf.len()-8)..], b"</html>\n");

        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());
    }

    #[test]
    fn test_record_convert_dialog() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/example.barc");
        let mut reader = bfile.reader().unwrap();
        let rc1 = reader.read(&tune).unwrap().unwrap();

        let dl: Dialog = rc1.clone().try_into().unwrap();
        let rc2: Record = dl.try_into().unwrap();
        assert_eq!(rc1.rec_type, rc2.rec_type);
        assert_eq!(rc1.meta, rc2.meta);
        assert_eq!(rc1.req_headers, rc2.req_headers);
        assert_eq!(rc1.req_body.len(), rc2.req_body.len());
        assert_eq!(rc1.res_headers, rc2.res_headers);
        assert_eq!(rc1.res_body.len(), rc2.res_body.len());
    }

    #[test]
    fn test_record_convert_dialog_204() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/204_no_body.barc");
        let mut reader = bfile.reader().unwrap();
        let rc1 = reader.read(&tune).unwrap().unwrap();

        let dl: Dialog = rc1.clone().try_into().unwrap();
        let rc2: Record = dl.try_into().unwrap();
        assert_eq!(rc1.rec_type, rc2.rec_type);
        assert_eq!(rc1.meta, rc2.meta);
        assert_eq!(rc1.req_headers, rc2.req_headers);
        assert_eq!(rc1.req_body.len(), rc2.req_body.len());
        assert_eq!(rc1.res_headers, rc2.res_headers);
        assert_eq!(rc1.res_body.len(), rc2.res_body.len());
    }

    #[test]
    fn test_read_sample_larger() {
        let record = {
            let tune = Tuner::new()
                .set_max_body_ram(1024) // < 1270 expected length
                .finish();

            let bfile = BarcFile::new("sample/example.barc");
            let mut reader = bfile.reader().unwrap();
            let r = reader.read(&tune).unwrap().unwrap();

            let next = reader.read(&tune).unwrap();
            assert!(next.is_none());
            r
        };

        println!("{:#?}", record);

        assert!(!record.res_body.is_ram());
        let mut br = record.res_body.reader();
        let mut buf = Vec::with_capacity(2048);
        br.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.len(), 1270);
        assert_eq!(&buf[0..15], b"<!doctype html>");
        assert_eq!(&buf[(buf.len()-8)..], b"</html>\n");
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn test_read_sample_mapped() {
        let mut record = {
            let tune = Tuner::new()
                .set_max_body_ram(1024) // < 1270 expected length
                .finish();

            let bfile = BarcFile::new("sample/example.barc");
            let mut reader = bfile.reader().unwrap();
            let r = reader.read(&tune).unwrap().unwrap();

            let next = reader.read(&tune).unwrap();
            assert!(next.is_none());
            r
        };
        record.res_body.mem_map().unwrap();

        println!("{:#?}", record);

        assert!(!record.res_body.is_ram());
        let mut br = record.res_body.reader();
        let mut buf = Vec::with_capacity(2048);
        br.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.len(), 1270);
        assert_eq!(&buf[0..15], b"<!doctype html>");
        assert_eq!(&buf[(buf.len()-8)..], b"</html>\n");
    }

    #[test]
    fn test_read_empty_file() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/empty.barc");
        let mut reader = bfile.reader().unwrap();
        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());

        // Shouldn't have moved
        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());
    }

    #[test]
    fn test_read_over_reserved() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/reserved.barc");
        let mut reader = bfile.reader().unwrap();
        let record = reader.read(&tune).unwrap();

        println!("{:#?}", record);

        assert!(record.is_none());

        // Should seek back to do it again
        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());
    }

    #[test]
    fn test_read_short_record_head() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/reserved.barc");
        let mut reader = bfile.reader().unwrap();

        // Seek to bad position
        reader.seek(1).unwrap();

        if let Err(e) = reader.read(&tune) {
            if let BarcError::ReadIncompleteRecHead(l) = e {
                assert_eq!(l, V2_HEAD_SIZE - 1);
                let em = e.to_string();
                assert!(em.contains("Incomplete"), em)
            } else {
                panic!("Other error: {}", e);
            }
        } else {
            panic!("Should not succeed!");
        }
    }

    #[test]
    fn test_read_bad_record_head() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/example.barc");
        let mut reader = bfile.reader().unwrap();

        // Seek to bad position
        reader.seek(1).unwrap();

        if let Err(e) = reader.read(&tune) {
            if let BarcError::ReadInvalidRecHead = e {
                assert!(true);
            } else {
                panic!("Other error: {}", e);
            }
        } else {
            panic!("Should not succeed!");
        }
    }

    #[test]
    fn test_read_truncated() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/truncated.barc");
        let mut reader = bfile.reader().unwrap();
        if let Err(e) = reader.read(&tune) {
            if let BarcError::Io(ioe) = e {
                assert_eq!(ErrorKind::UnexpectedEof, ioe.kind());
            } else {
                panic!("Other error type {:?}", e);
            }
        } else {
            panic!("Should not succeed!");
        }
    }

    #[test]
    fn test_read_204_no_body() {
        let tune = Tunables::new();
        let bfile = BarcFile::new("sample/204_no_body.barc");
        let mut reader = bfile.reader().unwrap();
        let record = reader.read(&tune).unwrap().unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 4);
        assert_eq!(record.req_headers.len(), 4);
        assert!(record.req_body.is_empty());
        assert_eq!(record.res_headers.len(), 9);

        assert!(record.res_body.is_empty());
    }
}
