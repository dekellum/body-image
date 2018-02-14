#![allow(dead_code)]

extern crate failure;
extern crate http;

use failure::Error as FlError;
use std::io::{Seek, SeekFrom, Write};
use std::fs::{File, OpenOptions};
use std::sync::{RwLock, RwLockWriteGuard};
use std::path::Path;

use super::Dialog;

struct BarcFile {
    lock: RwLock<BarcFileInner>
}

struct BarcFileInner {
    file: File,
}

struct BarcWriter<'a> {
    guard: RwLockWriteGuard<'a, BarcFileInner>
}

impl BarcFile {
    fn open<P>(path: P) -> Result<BarcFile, FlError>
        where P: AsRef<Path>
    {
        let file =
            OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(BarcFile { lock: RwLock::new(BarcFileInner { file }) })
    }

    fn writer(&self) -> Result<BarcWriter, FlError> {
        let guard = self.lock.write().unwrap(); // FIXME:
        // PoisonError is not send, so can't map to FlError
        Ok(BarcWriter { guard })
    }
}

impl<'a> BarcWriter<'a> {

    fn write(&mut self, dialog: Dialog) -> Result<(), FlError>
    {
        let inner = &mut *self.guard;
        let fout = &mut inner.file;

        // Write initial head as reserved place holder
        let head  = Self::write_record_place_holder(fout)?;
        // FIXME: Write meta section
        let req_h = Self::write_headers(fout, &dialog.req_headers)?;
        // FIXME: Write request body (if any)
        let res_h = Self::write_headers(fout, &dialog.res_headers)?;
        // FIXME: Write response body (if any)

        // Compute total, excluding the fixed head length
        let total_ex = req_h + res_h;

        // Seek back and write final record head
        fout.seek(SeekFrom::Current(-((total_ex + head) as i64)))?;
        Self::write_record_head(
            fout,
            total_ex as u64,
            'H',
            'P',
            0u16, // FIXME: meta
            req_h as u16,
            0u32, // FIXME: req body
            res_h as u16)?;
        fout.seek(SeekFrom::End(0))?;

        Ok(())
    }

    fn write_record_place_holder(out: &mut Write) -> Result<usize, FlError> {
        Self::write_record_head(out, 0u64, 'R', 'U', 0u16, 0u16, 0u32, 0u16)
    }

    fn write_record_head(
        out: &mut Write,
        len: u64,
        type_f: char,
        cmpr_f: char,
        meta: u16,
        req_h: u16,
        req_b: u32,
        res_h: u16) -> Result<usize, FlError>
    {
        let size = out.write(format!(
            "BARCv2 {:016x} {}{} {:04x} {:04x} {:08x} {:04x}\r\n\r\n",
            len, type_f, cmpr_f, meta, req_h, req_b, res_h
        ).as_bytes())?;
        assert_eq!(size, 36+1+8+8+1);
        Ok(size)
    }

    fn write_headers(out: &mut Write, headers: &http::HeaderMap)
        -> Result<usize, FlError>
    {
        let mut size = 0;
        for (key, value) in headers.iter() {
            size += out.write(key.as_ref())?;
            size += out.write(b": ")?;
            size += out.write(value.as_bytes())?;
            size += out.write(b"\r\n")?;
        }
        size += out.write(b"\r\n")?;
        Ok(size)
    }
}
