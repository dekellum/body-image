#![allow(dead_code)]

extern crate failure;
extern crate http;

use failure::Error as FlError;
use std::io::{/*Seek, SeekFrom,*/ Write};
use std::fs::File;

struct BarcFile {
    file: File,
}

struct BarcWriter {}

impl BarcWriter {

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
