//! Basic Archive (BARC) file format reader and writer.

extern crate bytes;
extern crate http;
extern crate httparse;
extern crate memmap;
extern crate flate2;

use std::cmp;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::ops::{AddAssign,ShlAssign};
use std::sync::{Mutex, MutexGuard};
use std::path::Path;

use self::bytes::{BytesMut, BufMut};
use failure::Error as FlError;
use self::flate2::Compression as GzCompression;
use self::flate2::write::GzEncoder;
use self::flate2::read::GzDecoder;
use hyper::Chunk;
use http::header::{HeaderName, HeaderValue};
use memmap::MmapOptions;

use super::{BodyImage, Dialog, Mapped, Recorded, RequestRecorded, Tunables};
use super::compress::read_to_body;

/// Fixed record head size including CRLF terminator:
/// 54 Bytes
pub const V2_HEAD_SIZE: usize = 54;

/// Maximum total record length, excluding the record head:
/// 2<sup>48</sup> (256 TiB) - 1.
/// Note: this exceeds the file or partition size limits of many
/// filesystems.
pub const V2_MAX_RECORD: u64 = 0xfff_fff_fff_fff;

/// Maximum header (meta, request, response) block size, including
/// CRLF terminator:
/// 2<sup>20</sup> (1 MiB) - 1.
pub const V2_MAX_HBLOCK: usize =        0xff_fff;

/// Maximum request body size, including any CRLF terminator:
/// 2<sup>40</sup> (1 TiB) - 1.
pub const V2_MAX_REQ_BODY: u64 = 0xf_fff_fff_fff;

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
    file: File
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

/// An owned BARC record.
#[derive(Debug, Default)]
pub struct Record {
    rec_type:         RecordType,
    meta:             http::HeaderMap,
    req_headers:      http::HeaderMap,
    req_body:         BodyImage,
    res_headers:      http::HeaderMap,
    res_body:         BodyImage,
}

/// Access to BARC record-like objects by reference. Extends
/// `Recorded`.
pub trait RecordedType: Recorded {
    /// Record type.
    fn rec_type(&self)    -> RecordType;
}

impl RequestRecorded for Record {
    fn req_headers(&self) -> &http::HeaderMap  { &self.req_headers }
    fn req_body(&self)    -> &BodyImage        { &self.req_body }
}

impl Recorded for Record {
    fn meta(&self)        -> &http::HeaderMap  { &self.meta }
    fn res_headers(&self) -> &http::HeaderMap  { &self.res_headers }
    fn res_body(&self)    -> &BodyImage        { &self.res_body }
}

impl RecordedType for Record {
    fn rec_type(&self)    -> RecordType        { self.rec_type }
}

impl RecordedType for Dialog {
    fn rec_type(&self)    -> RecordType        { RecordType::Dialog }
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
    fn flag(self) -> char {
        match self {
            RecordType::Reserved => 'R',
            RecordType::Dialog => 'D',
        }
    }

    fn try_from(f: u8) -> Result<Self, FlError> {
        match f {
            b'R' => Ok(RecordType::Reserved),
            b'D' => Ok(RecordType::Dialog),
            _ => Err(format_err!("Unknown record type flag [{}]", f))
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
}

impl Compression {
    fn flag(self) -> char {
        match self {
            Compression::Plain   => 'P',
            Compression::Gzip    => 'G',
        }
    }

    fn try_from(f: u8) -> Result<Self, FlError> {
        match f {
            b'P' => Ok(Compression::Plain),
            b'G' => Ok(Compression::Gzip),
            _ => Err(format_err!("Unknown compression flag [{}]", f))
        }
    }
}

const CRLF: &[u8] = b"\r\n";

const V2_RESERVE_HEAD: RecordHead = RecordHead {
    len: 0,
    rec_type: RecordType::Reserved,
    compress: Compression::Plain,
    meta: 0,
    req_h: 0,
    req_b: 0,
    res_h: 0
};

pub trait WriteStrategy {
    fn wrap<'a>(&self, body_len: u64, file: &'a File)
        -> Result<WriteWrapper<'a>, FlError>;
}

pub struct GzipWriteStrategy {
    min_len: u64,
    compression_level: u32,
}

impl Default for GzipWriteStrategy {
    fn default() -> Self {
        Self { min_len: 8 * 1024,
               compression_level: 6 }
    }
}

impl WriteStrategy for GzipWriteStrategy {
    fn wrap<'a>(&self, body_len: u64, file: &'a File)
        -> Result<WriteWrapper<'a>, FlError>
    {
        if body_len >= self.min_len {
            Ok(WriteWrapper::Gzip(
                GzEncoder::new(file, GzCompression::new(self.compression_level))
            ))
        } else {
            Ok(WriteWrapper::Plain(file))
        }
    }
}

pub struct PlainWriteStrategy {}

impl Default for PlainWriteStrategy {
    fn default() -> Self { Self {} }
}

impl WriteStrategy for PlainWriteStrategy {
    fn wrap<'a>(&self, _body_len: u64, file: &'a File)
        -> Result<WriteWrapper<'a>, FlError>
    {
        Ok(WriteWrapper::Plain(file))
    }
}

pub enum WriteWrapper<'a> {
    Plain(&'a File),
    Gzip(GzEncoder<&'a File>)
}

impl<'a> WriteWrapper<'a> {
    /// Return the Compression flag varient in use
    pub fn mode(&self) -> Compression {
        match *self {
            WriteWrapper::Plain(_) => Compression::Plain,
            WriteWrapper::Gzip(_) => Compression::Gzip
        }
    }

    /// Return a Write reference for self
    pub fn as_write(&mut self) -> &mut Write {
        match *self {
            WriteWrapper::Plain(ref mut f) => f,
            WriteWrapper::Gzip(ref mut gze) => gze
        }
    }

    pub fn finish(self) -> Result<(), FlError> {
        match self {
            WriteWrapper::Plain(_) => Ok(()),
            WriteWrapper::Gzip(gze) => {
                gze.finish()?.flush()?;
                Ok(())
            }
        }
    }
}

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
    pub fn writer(&self) -> Result<BarcWriter, FlError> {
        let mut guard = self.write_lock.lock().unwrap(); // FIXME:
        // PoisonError is not send, so can't map to FlError

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

    /// Get a reader for this file. Errors if the file does not
    /// exist.
    pub fn reader(&self) -> Result<BarcReader, FlError> {
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)?;
        // FIXME: Use fs2 crate for: file.try_lock_shared()?
        Ok(BarcReader { file })
    }
}

impl<'a> BarcWriter<'a> {
    /// Write a new record, returning the record's offset from the
    /// start of the BARC file. The writer position is then advanced
    /// to the end of the file, for the next `write`.
    pub fn write<R>(&mut self, rec: &R, strategy: &WriteStrategy)
        -> Result<u64, FlError>
        where R: RecordedType
    {
        // BarcFile::writer() guarantees Some(File)
        let file = &mut *self.guard.as_mut().unwrap();

        // Write initial head as reserved place holder
        let start = file.seek(SeekFrom::End(0))?;
        write_record_head(file, &V2_RESERVE_HEAD)?;
        file.flush()?;

        let size_est = rec.req_body().len() + rec.res_body().len();
        let mut head = {
            let mut wrapper = strategy.wrap(size_est, file)?;
            let compress = wrapper.mode();
            let with_crlf = compress == Compression::Plain;
            let head = {
                let fout = wrapper.as_write();

                let meta = write_headers(fout, rec.meta())?;

                let req_h = write_headers(fout, rec.req_headers())?;
                let req_b = write_body(fout, with_crlf, rec.req_body())?;

                let res_h = write_headers(fout, rec.res_headers())?;

                // Compute total thus far, excluding the fixed head length
                let mut len: u64 = (meta + req_h + res_h) as u64 + req_b;

                assert!((len + rec.res_body().len() + 2) <= V2_MAX_RECORD,
                        "body exceeds size limit");

                let res_b = write_body(fout, with_crlf, rec.res_body())?;
                len += res_b;

                RecordHead {
                    len, // adjusted below
                    rec_type: rec.rec_type(),
                    compress,
                    meta,
                    req_h,
                    req_b,
                    res_h }
            };

            wrapper.finish()?;
            head
        };

        // Use new file offset to indicate total length
        let end = file.seek(SeekFrom::Current(0))?;
        let orig_len = head.len;
        head.len = end - start - (V2_HEAD_SIZE as u64);
        if head.compress == Compression::Plain {
            assert_eq!(orig_len, head.len);
        } else if orig_len < head.len {
            println!("WARN: Compression grew record from {} to {} bytes",
                     orig_len, head.len);
        }

        // Seek back and write final record head, with known sizes
        file.seek(SeekFrom::Start(start))?;
        write_record_head(file, &head)?;

        // Seek to end and flush
        file.seek(SeekFrom::End(0))?;
        file.flush()?;

        Ok(start)
    }
}

fn write_record_head(out: &mut Write, head: &RecordHead)
    -> Result<(), FlError>
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

fn write_headers(out: &mut Write, headers: &http::HeaderMap)
    -> Result<usize, FlError>
{
    let mut size = 0;
    for (key, value) in headers.iter() {
        size += write_all_len(out, key.as_ref())?;
        size += write_all_len(out, b": ")?;
        size += write_all_len(out, value.as_bytes())?;
        size += write_all_len(out, CRLF)?;
    }
    if size > 0 {
        size += write_all_len(out, CRLF)?;
    }
    assert!(size <= V2_MAX_HBLOCK);
    Ok(size)
}

fn write_body(out: &mut Write, with_crlf: bool, body: &BodyImage)
    -> Result<u64, FlError>
{
    let mut size = body.write_to(out)?;
    if with_crlf && size > 0 {
        size += write_all_len(out, CRLF)? as u64;
    }
    Ok(size)
}

fn write_all_len(out: &mut Write, bs: &[u8]) -> Result<usize, FlError> {
    out.write_all(bs)?;
    Ok(bs.len())
}

impl BarcReader {

    /// Read and return the next Record or None if EOF. The provided
    /// Tunables `max_body_ram` controls, depending on record sizes,
    /// whether the request and response bodies are read directly
    /// intro RAM or returned as memory mapped regions.
    pub fn read(&mut self, tune: &Tunables)
        -> Result<Option<Record>, FlError>
    {
        let fin = &mut self.file;

        // Record start position in case we need to rewind
        let start = fin.seek(SeekFrom::Current(0))?;

        let rhead = match read_record_head(fin) {
            Ok(Some(rh)) => rh,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e)
        };

        let rec_type = rhead.rec_type;

        // With a concurrent writer, its possible to see an
        // incomplete, Reserved record head, followed by an empty or
        // partial record payload.  In this case, seek back to start
        // and return None.
        if rec_type == RecordType::Reserved {
            fin.seek(SeekFrom::Start(start))?;
            return Ok(None);
        }

        if rhead.compress != Compression::Plain {
            let rec = read_compressed(fin, &rhead, tune)?;
            return Ok(Some(rec))
        }

        let meta = read_headers(fin, rhead.meta)?;

        let req_headers = read_headers(fin, rhead.req_h)?;
        let mut total: u64 = (rhead.meta + rhead.req_h) as u64;

        let req_body = if rhead.req_b <= tune.max_body_ram() {
            read_body_ram(fin, true, rhead.req_b as usize)
        } else {
            let offset = start + (V2_HEAD_SIZE as u64) + total;
            map_body(fin, offset, rhead.req_b)
        }?;

        let res_headers = read_headers(fin, rhead.res_h)?;
        total += rhead.req_b;
        total += rhead.res_h as u64;
        let body_len = rhead.len - total;

        let res_body = if body_len <= tune.max_body_ram() {
            read_body_ram(fin, true, body_len as usize)
        } else {
            let offset = start + (V2_HEAD_SIZE as u64) + total;
            map_body(fin, offset, body_len)
        }?;

        Ok(Some(Record { rec_type, meta, req_headers, req_body,
                         res_headers, res_body }))
    }

    /// Seek to a known offset (e.g. 0 or returned from
    /// `BarcWriter::write` for a specific record) from the start of
    /// the BARC file. This effects subsequent calls to `read`, which
    /// may error if the position is not to a valid record head.
    pub fn seek(&mut self, offset: u64) -> Result<(), FlError> {
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(())
    }
}

fn read_compressed(file: &mut File, rhead: &RecordHead, tune: &Tunables)
    -> Result<Record, FlError>
{
    assert!(rhead.compress == Compression::Gzip);

    // Decoder over limited `Take` of compressed record len
    let fin = &mut GzDecoder::new(file.take(rhead.len));

    let meta = read_headers(fin, rhead.meta)?;

    let req_headers = read_headers(fin, rhead.req_h)?;

    let req_body = if rhead.req_b <= tune.max_body_ram() {
        read_body_ram(fin, false, rhead.req_b as usize)?
    } else {
        read_body_fs(fin, rhead.req_b, tune)?.prepare()?
    };

    let res_headers = read_headers(fin, rhead.res_h)?;

    // When compressed, we don't actually know the final size of the
    // response body. Estimate and use compress::read_to_body, which
    // may return `Ram` or `FsWrite` states, so also prepare it.
    let est = fin.get_ref().limit() * u64::from(tune.size_estimate_gzip())
              + 4_096;
    println!( "Estimated body: {}", est);
    let res_body = read_to_body(fin, est, tune)?.prepare()?;

    Ok(Record { rec_type: rhead.rec_type,
                meta, req_headers, req_body, res_headers, res_body })
}

// Return RecordHead or None if EOF
fn read_record_head(r: &mut Read)
    -> Result<Option<RecordHead>, FlError>
{
    let mut buf = [0u8; V2_HEAD_SIZE];

    let size = read_record_head_buf(r, &mut buf)?;
    if size == 0 {
        return Ok(None);
    }
    if size != V2_HEAD_SIZE {
        bail!("Incomplete header len {}", size);
    }
    if &buf[0..6] != b"BARC2 " {
        bail!("Invalid header suffix");
    }

    let len       = parse_hex(&buf[6..18])?;
    let rec_type  = RecordType::try_from(buf[19])?;
    let compress  = Compression::try_from(buf[20])?;
    let meta      = parse_hex(&buf[22..27])?;
    let req_h     = parse_hex(&buf[28..33])?;
    let req_b     = parse_hex(&buf[34..44])?;
    let res_h     = parse_hex(&buf[45..50])?;
    Ok(Some(RecordHead { len, rec_type, compress, meta, req_h, req_b, res_h }))
}

// Like `Read::read_exact` but we need to distinguish 0 bytes read
// (EOF) from partial bytes read (a format error), so it also returns
// the number of bytes read.
fn read_record_head_buf(r: &mut Read, mut buf: &mut [u8])
    -> Result<usize, FlError>
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
fn parse_hex<T>(buf: &[u8]) -> Result<T, FlError>
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
            bail!("Illegal head hex digit: [{}]", d);
        }
    }
    Ok(v)
}

fn read_headers(r: &mut Read, len: usize)
    -> Result<http::HeaderMap, FlError>
{
    if len == 0 {
        return Ok(http::HeaderMap::with_capacity(0));
    }

    assert!(len > 2);

    let mut buf = BytesMut::with_capacity(len);
    unsafe {
        r.read_exact(&mut buf.bytes_mut()[..len])?;
        buf.advance_mut(len);
    }

    // Don't exclude trailing CRLF, as its used to signal end of
    // headers (avoids Partial)
    parse_headers(&buf[..])
}

fn parse_headers(buf: &[u8]) -> Result<http::HeaderMap, FlError> {
    let mut headbuf = [httparse::EMPTY_HEADER; 128];
    // FIXME: parse_headers API will return TooManyHeaders if headbuf
    // isn't large enough. Hyper 0.11.15 allocates 100, so 128 is room
    // for "even more" (sarcasm). Might be better to just replace this
    // with our own parser, as the grammer isn't particularly complex.

    match httparse::parse_headers(buf, &mut headbuf) {
        Ok(httparse::Status::Complete((size, heads))) => {
            let mut hmap = http::HeaderMap::with_capacity(heads.len());
            assert_eq!(size, buf.len());
            for h in heads {
                hmap.append(h.name.parse::<HeaderName>()?,
                            HeaderValue::from_bytes(h.value)?);
            }
            Ok(hmap)
        }
        Ok(httparse::Status::Partial) => {
            bail!("Header block not terminated with blank line")
        }
        Err(e) => Err(FlError::from(e))
    }
}

// Read into `BodyImage` of state `Ram` as a single-chunk.
fn read_body_ram(r: &mut Read, with_crlf: bool, len: usize)
    -> Result<BodyImage, FlError>
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

    let chunk: Chunk = buf.freeze().into();
    let mut b = BodyImage::with_chunks_capacity(1);
    b.save(chunk)?;
    Ok(b)
}

// Read into `BodyImage` state `FsWrite`. Assumes no CRLF terminator
// (only used for compressed records).
fn read_body_fs(r: &mut Read, len: u64, tune: &Tunables)
    -> Result<BodyImage, FlError>
{
    if len == 0 {
        return Ok(BodyImage::empty());
    }

    let mut body = BodyImage::with_fs()?;
    let mut buf = BytesMut::with_capacity(tune.decode_buffer_fs());
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
        println!("Write (Fs) decoded buf rlen {}", rlen);
        body.write_all(&buf)?;

        if body.len() < len {
            buf.clear();
        }
        else {
            assert_eq!(body.len(), len);
            break;
        }
    }
    Ok(body)
}

// Return `BodyImage::MemMap` for an uncompressed body in file, at
// offset and length. Assumes (and asserts that) current is positioned
// at offset, and seeks file past the body len.
fn map_body(file: &mut File, offset: u64, len: u64)
    -> Result<BodyImage, FlError>
{
    assert!(len > 2);

    // Seek past the body, as if read.
    let end = file.seek(SeekFrom::Current(len as i64))?;
    assert_eq!(offset + len, end);

    let dup_file = file.try_clone()?;

    let map = unsafe {
        MmapOptions::new()
            .offset(offset as usize)
            .len((len - 2) as usize) // Exclude final CRLF
            .map(&dup_file)?
    };

    Ok(BodyImage::with_map(Mapped { map, _file: dup_file }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use http::header::{AGE, REFERER, VIA};
    use super::*;
    use super::super::Tuner;

    fn barc_test_file(name: &str) -> Result<PathBuf, FlError> {
        let tpath = Path::new("target/testmp");
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
        let strategy = PlainWriteStrategy::default();
        write_read_small(&fname, &strategy).unwrap();
    }

    #[test]
    fn test_write_read_small_gzip() {
        let fname = barc_test_file("small_gzip.barc").unwrap();
        let mut strategy = GzipWriteStrategy::default();
        strategy.min_len = 0;
        write_read_small(&fname, &strategy).unwrap();
    }

    fn write_read_small(fname: &PathBuf, strategy: &WriteStrategy)
        -> Result<(), FlError>
    {
        let bfile = BarcFile::new(fname);

        let req_body_str = "REQUEST BODY";
        let res_body_str = "RESPONSE BODY";

        let rec_type = RecordType::Dialog;
        let mut meta = http::HeaderMap::new();
        meta.insert(AGE, "0".parse()?);

        let mut req_headers = http::HeaderMap::new();
        req_headers.insert(REFERER, "http:://other.com".parse()?);
        let mut req_body = BodyImage::with_chunks_capacity(1);
        req_body.save(req_body_str.into())?;

        let mut res_headers = http::HeaderMap::new();
        res_headers.insert(VIA, "test".parse()?);
        let mut res_body = BodyImage::with_chunks_capacity(1);
        res_body.save(res_body_str.into())?;

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
        let strategy = PlainWriteStrategy::default();
        write_read_empty_record(&fname, &strategy).unwrap();;
    }

    #[test]
    fn test_write_read_empty_record_gzip() {
        let fname = barc_test_file("empty_record_gzip.barc").unwrap();
        let mut strategy = GzipWriteStrategy::default();
        strategy.min_len = 0;
        write_read_empty_record(&fname, &strategy).unwrap();
    }

    fn write_read_empty_record(fname: &PathBuf, strategy: &WriteStrategy)
        -> Result<(), FlError>
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
        let strategy = PlainWriteStrategy::default();
        write_read_large(&fname, &strategy).unwrap();;
    }

    #[test]
    fn test_write_read_large_gzip() {
        let fname = barc_test_file("large_gzip.barc").unwrap();
        let strategy = GzipWriteStrategy::default();
        write_read_large(&fname, &strategy).unwrap();
    }

    fn write_read_large(fname: &PathBuf, strategy: &WriteStrategy)
        -> Result<(), FlError>
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

        let mut req_body = BodyImage::with_chunks_capacity(500);
        for _ in 0..500 {
            req_body.save(lorem_ipsum.into())?;
        }

        let mut res_body = BodyImage::with_chunks_capacity(1_000);
        for _ in 0..1_000 {
            res_body.save(lorem_ipsum.into())?;
        }

        writer.write(&Record { req_body, res_body, ..Record::default()}, strategy)?;

        let tune = Tunables::new();
        let mut reader = bfile.reader()?;
        let record = reader.read(&tune)?.unwrap();

        println!("{:#?}", record);

        assert_eq!(record.rec_type, RecordType::Dialog);
        assert_eq!(record.meta.len(), 0);
        assert_eq!(record.req_headers.len(), 0);
        assert_eq!(record.req_body.len(), (lorem_ipsum.len() * 500) as u64);
        assert_eq!(record.res_headers.len(), 0);
        assert_eq!(record.res_body.len(), (lorem_ipsum.len() * 1_000) as u64);

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

        // Estabilish reader.
        let tune = Tunables::new();
        let mut reader = bfile.reader().unwrap();
        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());

        // Write record with new writer
        let mut writer = bfile.writer().unwrap();
        let mut res_body = BodyImage::with_chunks_capacity(1);
        res_body.save(res_body_str.into()).unwrap();

        let offset = writer.write(&Record {
            res_body, ..Record::default() },
            &PlainWriteStrategy::default()).unwrap();
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
                     &PlainWriteStrategy::default()).unwrap();

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
        let mut body_reader = record.res_body.reader();
        let br = body_reader.as_read();
        let mut buf = Vec::with_capacity(2048);
        br.read_to_end(&mut buf).unwrap();
        assert_eq!(buf.len(), 1270);
        assert_eq!(&buf[0..15], b"<!doctype html>");
        assert_eq!(&buf[(buf.len()-8)..], b"</html>\n");

        let record = reader.read(&tune).unwrap();
        assert!(record.is_none());
    }

    #[test]
    fn test_read_sample_mapped() {
        let record = {
            let mut tune = Tuner::new()
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
        let mut body_reader = record.res_body.reader();
        let br = body_reader.as_read();
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
            println!("{}", e);
            assert!(e.to_string().contains("Incomplete header"));
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
            println!("{}", e);
            assert!(e.to_string().contains("Invalid header suffix"));
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
