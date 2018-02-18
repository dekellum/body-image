extern crate failure;
extern crate http;

use failure::Error as FlError;
use std;
use std::io::{Seek, SeekFrom, Write};
use std::fs::{File, OpenOptions};
use std::sync::{RwLock, RwLockWriteGuard};
use std::path::Path;

use super::{BodyImage, Dialog};

pub struct BarcFile {
    lock: RwLock<BarcFileInner>
}

pub struct BarcFileInner {
    // FIXME: Each reader will need a new, independent File instance
    // openned read-only and closed when dropped, with its own
    // position. Save off the Path for this purpose.
    file: File,
}

pub struct BarcWriter<'a> {
    // FIXME: RwLock isn't a perfect fit, since it is possible from a
    // File level to support 1-writer and N-readers at the same time.
    guard: RwLockWriteGuard<'a, BarcFileInner>
}

impl BarcFile {
    pub fn open<P>(path: P) -> Result<BarcFile, FlError>
        where P: AsRef<Path>
    {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        Ok(BarcFile { lock: RwLock::new(BarcFileInner { file }) })
    }

    pub fn writer(&self) -> Result<BarcWriter, FlError> {
        let guard = self.lock.write().unwrap(); // FIXME:
        // PoisonError is not send, so can't map to FlError
        Ok(BarcWriter { guard })
    }
}

/// Fixed Record Head size in bytes, for version 2
const BARC_2_HEAD_SIZE: usize = 54;

impl<'a> BarcWriter<'a> {

    pub fn write(&mut self, dialog: &Dialog) -> Result<(), FlError>
    {
        let inner = &mut *self.guard;
        let fout = &mut inner.file;

        // Write initial head as reserved place holder
        let start = fout.seek(SeekFrom::End(0))?;
        write_record_place_holder(fout)?;

        // FIXME: Should probably externalize meta, so users can add
        // whatever desirend, and possibly providing a convenience
        // "derive_meta" off of Dialog?
        let meta = derive_meta(dialog)?;
        let meta_h = write_headers(fout, &meta)?;

        let req_h = write_headers(fout, &dialog.req_headers)?;
        // FIXME: Write any request body (e.g. POST) when available

        let res_h = write_headers(fout, &dialog.res_headers)?;

        let res_b = write_body(fout, &dialog.body)?;

        // Compute total, excluding the fixed head length
        let total_ex: u64 = (meta_h + req_h + res_h) as u64 + res_b;

        // Seek back and write final record head, with known sizes
        fout.seek(SeekFrom::Start(start))?;
        write_record_head(
            fout,
            total_ex,
            'H',  // FIXME: option?
            'P',  // FIXME: compression support
            meta_h as u16,
            req_h as u16,
            0u32, // FIXME: req body
            res_h as u16)?;

        fout.seek(SeekFrom::End(0))?;

        Ok(())
    }
}

fn derive_meta(dialog: &Dialog) -> Result<http::HeaderMap, FlError> {
    let mut hs = http::HeaderMap::new();
    hs.append("url", dialog.url.to_string().parse()?);
    hs.append("method", dialog.method.to_string().parse()?);

    // FIXME: Rely on debug format of version for now. Should probably
    // replace this with match and custom representation.
    let v = format!("{:?}", dialog.version);
    hs.append("response-version", v.parse()?);

    hs.append("response-status",  dialog.status.to_string().parse()?);
    Ok(hs)
}

fn write_record_place_holder(out: &mut Write) -> Result<(), FlError> {
    write_record_head(out, 0u64, 'R', 'U', 0u16, 0u16, 0u32, 0u16)
}

#[cfg_attr(feature = "cargo-clippy", allow(too_many_arguments))]
fn write_record_head(
    out: &mut Write,
    len: u64,
    type_f: char,
    cmpr_f: char,
    meta: u16,
    req_h: u16,
    req_b: u32,
    res_h: u16) -> Result<(), FlError>
{
    let size = write_all_len(out, format!(
        // ----7------24---27-----32-----37-----46----50------54
        "BARC 2 {:016x} {}{} {:04x} {:04x} {:08x} {:04x}\r\n\r\n",
        len, type_f, cmpr_f, meta, req_h, req_b, res_h
    ).as_bytes())?;
    assert_eq!(size, BARC_2_HEAD_SIZE, "BARC 2 record head size invariant");
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
    assert!(size <= (std::u16::MAX as usize));
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
