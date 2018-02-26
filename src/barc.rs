extern crate failure;
extern crate http;
extern crate httparse;
extern crate bytes;

use failure::Error as FlError;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::ops::{AddAssign,ShlAssign};
use std::sync::{Mutex, MutexGuard};
use std::path::Path;

use super::{BodyImage, Dialog};

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

/// Maximum request body size, including CRLF terminator:
/// 2<sup>40</sup> (1 TiB) - 1.
pub const V2_MAX_REQ_BODY: u64 = 0xf_fff_fff_fff;

pub struct BarcFile {
    path: Box<Path>,
    write_lock: Mutex<Option<File>>,
}

/// BARC File handle for write access
pub struct BarcWriter<'a> {
    guard: MutexGuard<'a, Option<File>>
}

/// BARC File handle for read access. Each reader has its own File
/// handle and position.
pub struct BarcReader {
    file: File
}

struct RecordHead {
    len:              u64,
    rec_type:         RecordType,
    compress:         Compression,
    meta:             usize,
    req_h:            usize,
    req_b:            u64,
    res_h:            usize,
}

pub struct Record {
    rec_type:         RecordType,
    meta:             http::HeaderMap,
    req_headers:      http::HeaderMap,
    req_body:         BodyImage,
    res_headers:      http::HeaderMap,
    res_body:         BodyImage,
}

#[derive(Clone, Copy, Debug)]
enum RecordType {
    Reserved,
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

#[derive(Clone, Copy, Debug)]
enum Compression {
    Unknown,
    Plain,
}

impl Compression {
    fn flag(self) -> char {
        match self {
            Compression::Unknown => 'U',
            Compression::Plain   => 'P',
        }
    }

    fn try_from(f: u8) -> Result<Self, FlError> {
        match f {
            b'U' => Ok(Compression::Unknown),
            b'P' => Ok(Compression::Plain),
            _ => Err(format_err!("Unknown compression flag [{}]", f))
        }
    }
}

const V2_RESERVE_HEAD: RecordHead = RecordHead {
    len: 0,
    rec_type: RecordType::Reserved,
    compress: Compression::Unknown,
    meta: 0,
    req_h: 0,
    req_b: 0,
    res_h: 0
};

impl BarcFile {
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

    pub fn write(&mut self, dialog: &Dialog) -> Result<(), FlError> {
        // BarcFile::writer() guarantees Some(fout)
        let fout = &mut *self.guard.as_mut().unwrap();

        // Write initial head as reserved place holder
        let start = fout.seek(SeekFrom::End(0))?;
        write_record_head(fout, &V2_RESERVE_HEAD)?;
        fout.flush()?;

        let meta = write_headers(fout, &dialog.meta)?;

        let req_h = write_headers(fout, &dialog.prolog.req_headers)?;
        let req_b = write_body(fout, &dialog.prolog.req_body)?;

        let res_h = write_headers(fout, &dialog.res_headers)?;

        // Compute total thus far, excluding the fixed head length
        let mut len: u64 = (meta + req_h + res_h) as u64 + req_b;

        assert!((len + dialog.body_len + 2) <= V2_MAX_RECORD,
                "body exceeds size limit");
        let res_b = write_body(fout, &dialog.body)?;

        len += res_b; // New total

        // Seek back and write final record head, with known sizes
        fout.seek(SeekFrom::Start(start))?;
        write_record_head( fout, &RecordHead {
            len,
            rec_type: RecordType::Dialog, // FIXME: option?
            compress: Compression::Plain, // FIXME: compression support
            meta,
            req_h,
            req_b,
            res_h })?;

        fout.seek(SeekFrom::End(0))?;
        fout.flush()?;
        Ok(())
    }
}

fn write_record_head(out: &mut Write, head: &RecordHead) -> Result<(), FlError>
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
        size += write_all_len(out, b"\r\n")?;
    }
    if size > 0 {
        size += write_all_len(out, b"\r\n")?;
    }
    assert!(size <= V2_MAX_HBLOCK);
    Ok(size)
}

fn write_body(out: &mut Write, body: &BodyImage)
    -> Result<u64, FlError>
{
    let mut size = body.write_to(out)?;
    if size > 0 {
        size += write_all_len(out, b"\r\n")? as u64;
    }
    Ok(size)
}

fn write_all_len(out: &mut Write, bs: &[u8]) -> Result<usize, FlError>
{
    out.write_all(bs)?;
    Ok(bs.len())
}

impl BarcReader {
    pub fn next(&mut self) -> Result<Option<Record>, FlError> {
        let fin = &mut self.file;

        let rhead = match read_record_head(fin) {
            Ok(Some(rh)) => rh,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e)
        };

        let rec_type = rhead.rec_type;
        // FIXME: Rewind and return None when RecordType::Reserved

        // FIXME: Compressed record support?

        let meta = read_headers(fin, rhead.meta)?;

        let req_headers = read_headers(fin, rhead.req_h)?;
        let req_body  = read_body_ram(fin, rhead.req_b as usize)?;

        let res_headers = read_headers(fin, rhead.res_h)?;

        let total: u64 = (rhead.meta + rhead.req_h + rhead.res_h) as u64 +
            rhead.req_b;
        let body_len = rhead.len - total;
        let res_body = read_body_ram(fin, body_len as usize)?;

        // FIXME: Support memory mapped bodies at some threshold size,
        // and decompression to tempfile.

        Ok(Some(Record { rec_type, meta, req_headers, req_body,
                         res_headers, res_body }))
    }
}

// Return RecordHead or None if EOF
fn read_record_head(r: &mut Read)
    -> Result<Option<RecordHead>, FlError>
{
    // FIXME: Is uninitialized safe enough?
    use std::mem;
    let mut buf: [u8; V2_HEAD_SIZE] = unsafe { mem::uninitialized() };

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

    let len       = parse_hex(&buf[7..18])?;
    let rec_type  = RecordType::try_from(buf[20])?;
    let compress  = Compression::try_from(buf[21])?;
    let meta      = parse_hex(&buf[23..27])?;
    let req_h     = parse_hex(&buf[29..33])?;
    let req_b     = parse_hex(&buf[35..44])?;
    let res_h     = parse_hex(&buf[46..50])?;
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
            bail!("Illegal hex digit: [{}]", d);
        }
    }
    Ok(v)
}

fn read_body_ram(r: &mut Read, len: usize) -> Result<BodyImage, FlError> {
    use self::bytes::{BytesMut, BufMut};
    use hyper::Chunk;

    if len == 0 {
        return Ok(BodyImage::empty());
    }

    assert!( len > 2 );

    let mut buf = BytesMut::with_capacity(len);
    r.read_exact(unsafe { buf.bytes_mut() })?;
    // Exclude last CRLF ----------v
    unsafe { buf.advance_mut(len - 2) };

    let chunk: Chunk = buf.freeze().into();
    let mut b = BodyImage::with_chunks_capacity(1);
    b.save(chunk)?;
    Ok(b)
}

fn read_headers(r: &mut Read, len: usize) -> Result<http::HeaderMap, FlError> {
    use self::bytes::{BytesMut, BufMut};

    if len == 0 {
        return Ok(http::HeaderMap::with_capacity(0));
    }

    assert!( len > 2 );

    let mut buf = BytesMut::with_capacity(len);
    r.read_exact(unsafe { buf.bytes_mut() })?;
    unsafe { buf.advance_mut(len) };
    // FIXME: Exclude last CRLF? ---v
    parse_headers(&buf[..(buf.len()-2)])
}

fn parse_headers(buf: &[u8]) -> Result<http::HeaderMap, FlError> {
    use http::header::{HeaderName, HeaderValue};

    let mut headbuf = [httparse::EMPTY_HEADER; 128]; // FIXME: loop instead?
    match httparse::parse_headers(buf, &mut headbuf) {
        Ok(httparse::Status::Complete((size, heads))) => {
            let mut hmap = http::HeaderMap::with_capacity(heads.len());
            assert_eq!(size, buf.len());
            for h in heads {
                hmap.append(h.name.parse::<HeaderName>()?,
                            HeaderValue::from_bytes(h.value)?);
            }
            Ok(hmap)
        },
        Ok(httparse::Status::Partial) => bail!("partial headers?"),
        Err(e) => Err(FlError::from(e))
    }
}
