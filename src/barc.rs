extern crate failure;
extern crate http;

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

    pub fn write(&mut self, dialog: &Dialog) -> Result<(), FlError>
    {
        // BarcFile::writer() guarantees Some(fout)
        let fout = &mut *self.guard.as_mut().unwrap();

        // Write initial head as reserved place holder
        let start = fout.seek(SeekFrom::End(0))?;
        write_record_place_holder(fout)?;
        fout.flush()?;

        let meta_h = write_headers(fout, &dialog.meta)?;

        let req_h = write_headers(fout, &dialog.req_headers)?;
        // FIXME: Write any request body (e.g. POST) when available

        let res_h = write_headers(fout, &dialog.res_headers)?;

        // Compute total thus far, excluding the fixed head length
        let mut total_ex: u64 = (meta_h + req_h + res_h) as u64;

        assert!((total_ex + dialog.body_len + 2) <= V2_MAX_RECORD,
                "body exceeds size limit");
        let res_b = write_body(fout, &dialog.body)?;

        total_ex += res_b; // New total

        // Seek back and write final record head, with known sizes
        fout.seek(SeekFrom::Start(start))?;
        write_record_head(
            fout,
            total_ex,
            'H',  // FIXME: option?
            'P',  // FIXME: compression support
            meta_h,
            req_h,
            0u64, // FIXME: req body
            res_h)?;

        fout.seek(SeekFrom::End(0))?;
        fout.flush()?;
        Ok(())
    }
}

fn write_record_place_holder(out: &mut Write) -> Result<(), FlError> {
    write_record_head(out, 0, 'R', 'U', 0, 0, 0, 0)
}

#[cfg_attr(feature = "cargo-clippy", allow(too_many_arguments))]
fn write_record_head(
    out: &mut Write,
    len:    u64,
    type_f: char,
    cmpr_f: char,
    meta:   usize,
    req_h:  usize,
    req_b:  u64,
    res_h:  usize) -> Result<(), FlError>
{
    // Check input ranges
    assert!(len   <= V2_MAX_RECORD,   "len exceeded");
    assert!(type_f.is_ascii(),        "type_f not ascii");
    assert!(cmpr_f.is_ascii(),        "cmpr_f not ascii");
    assert!(meta  <= V2_MAX_HBLOCK,   "meta exceeded");
    assert!(req_h <= V2_MAX_HBLOCK,   "req_h exceeded");
    assert!(req_b <= V2_MAX_REQ_BODY, "req_b exceeded");
    assert!(res_h <= V2_MAX_HBLOCK,   "res_h exceeded");

    let size = write_all_len(out, format!(
        // ---6------19---22-----28-----34------45----50------54
        "BARC2 {:012x} {}{} {:05x} {:05x} {:010x} {:05x}\r\n\r\n",
        len, type_f, cmpr_f, meta, req_h, req_b, res_h
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
    // FIXME: Use TryFrom here and for all u* conversions, when its lands...
    // https://github.com/rust-lang/rfcs/pull/1542
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
}

struct RecordHead {
    len:    u64,
    type_f: char,
    cmpr_f: char,
    meta:   u32,
    req_h:  u32,
    req_b:  u64,
    res_h:  u32,
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
        bail!( "Incomplete header len {}", size );
    }
    if &buf[0..6] != b"BARC2 " {
        bail!( "Invalid header suffix" );
    }

    let len       = parse_hex(&buf[7..18])?;
    let type_f    = char::from(buf[20]);
    let cmpr_f    = char::from(buf[21]);
    let meta      = parse_hex(&buf[23..27])?;
    let req_h     = parse_hex(&buf[29..33])?;
    let req_b     = parse_hex(&buf[35..44])?;
    let res_h     = parse_hex(&buf[46..50])?;
    Ok(Some(RecordHead { len, type_f, cmpr_f, meta, req_h, req_b, res_h }))
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
            bail!( "Illegal hex digit: [{}]", d);
        }
    }
    Ok(v)
}
