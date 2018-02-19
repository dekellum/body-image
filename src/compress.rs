extern crate failure;
extern crate flate2;
extern crate http;
extern crate bytes;

use failure::Error as FlError;
use self::bytes::{BytesMut, BufMut};
use self::flate2::read::GzDecoder;
use super::{BodyImage, Dialog};
use std::io::{ErrorKind, Read};

pub fn decode_body(dialog: &mut Dialog) -> Result<(), FlError> {
    let headers = &mut dialog.res_headers;

    let encodings = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .chain(headers
               .get_all(http::header::CONTENT_ENCODING)
               .iter());

    for v in encodings {
        if let Ok(s) = v.to_str() {
            if s.find("gzip").is_some() {
                let (newb, size) = {
                    println!("Body to decode: {:?}", dialog.body);
                    let mut reader = dialog.body.reader();
                    let mut decoder = GzDecoder::new(reader.as_read());
                    let len_est = dialog.body_len * 4;

                    let max_body_ram  =        96 * 1024; //FIXME: From where?
                    let max_body_len = 192 * 1024 * 1024;

                    let mut newb = if len_est > max_body_ram {
                        BodyImage::with_fs()?
                    } else {
                        BodyImage::with_ram(len_est)
                    };

                    let mut size: u64 = 0;
                    loop {
                        let mut buf = BytesMut::with_capacity(8 * 1024);
                        let len = match decoder.read( unsafe { buf.bytes_mut() } ) {
                            Ok(len) => len,
                            Err(e) => {
                                if e.kind() == ErrorKind::Interrupted {
                                    continue;
                                } else {
                                    return Err(e.into());
                                }
                            }
                        };
                        if len == 0 {
                            break;
                        }
                        // FIXME: Alt version to reuse buf when BodyImage::FsWrite?
                        println!("Decoded buf len {}", len);
                        unsafe { buf.advance_mut(len) };
                        size += len as u64;
                        if size > max_body_ram && newb.is_ram() {
                            newb = newb.write_back()?;
                        }
                        newb.save(buf.freeze().into())?;
                    }
                    (newb, size)
                };
                dialog.body = newb.prepare()?;
                println!("Body update: {:?}", dialog.body);
                dialog.body_len = size;
            }
        }
    }
    Ok(())
}
