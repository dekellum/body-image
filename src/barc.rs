extern crate failure;
extern crate http;

use failure::Error as FlError;
use std::io::{Seek, SeekFrom, Write};
use std::fs::{File, OpenOptions};
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

impl BarcFile {
    pub fn new<P>(path: P) -> BarcFile
        where P: AsRef<Path>
    {
        // Each reader will own an independent File instance openned
        // read-only and closed when dropped, with its own
        // position. Save off the Path for this purpose.
        let path: Box<Path> = path.as_ref().into();
        let write_lock = Mutex::new(None);
        BarcFile { path, write_lock }
    }

    /// Get a writer for this file, opening the file for write (and
    /// possibly erroring) if this is the first time called. May block
    /// on the write lock, as only one `BarcWriter` instance is
    /// allowed.
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
