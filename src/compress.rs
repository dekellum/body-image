extern crate failure;
extern crate flate2;
extern crate http;
extern crate hyper;
extern crate bytes;

use std::io::{ErrorKind, Read};
use failure::Error as FlError;
use self::bytes::{BytesMut, BufMut};
use self::flate2::read::{DeflateDecoder, GzDecoder};
use hyper::header::{ContentEncoding, Encoding, Header, Raw};
use super::{BodyImage, Dialog, Tunables};

pub fn decode_body(dialog: &mut Dialog, tune: &Tunables) -> Result<(), FlError> {
    let headers = &mut dialog.res_headers;

    let encodings = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .chain(headers
               .get_all(http::header::CONTENT_ENCODING)
               .iter());

    let mut compress = None;

    'headers: for v in encodings {
        // Content-Encoding includes Brotli (br) and is otherwise a
        // super-set of Transfer-Encoding, so parse that way for both.
        if let Ok(v) = ContentEncoding::parse_header(&Raw::from(v.as_bytes())) {
            for av in v.iter() {
                match *av {
                    Encoding::Gzip | Encoding::Deflate => {
                        compress = Some(av.clone()); // FIXME: sad clone
                        break 'headers;
                    }
                    _ => (),
                }
            }
        }
    }

    if let Some(ref comp) = compress {
        let (new_body, size) = {
            println!("Body to {:?} decode: {:?}", comp, dialog.body);
            let mut reader = dialog.body.reader();
            match *comp {
                Encoding::Gzip => {
                    let mut decoder = GzDecoder::new(reader.as_read());
                    let len_est = dialog.body_len *
                        u64::from(tune.gzip_size_x_est);
                    read_to_body(&mut decoder, len_est, tune)?
                }
                Encoding::Deflate => {
                    let mut decoder = DeflateDecoder::new(reader.as_read());
                    let len_est = dialog.body_len *
                        u64::from(tune.deflate_size_x_est);
                    read_to_body(&mut decoder, len_est, tune)?
                }
                _ => unreachable!("Not matched above: {:?}", comp)
            }
        };
        dialog.body = new_body.prepare()?;
        println!("Body update: {:?}", dialog.body);
        dialog.body_len = size;

        // FIXME: Add meta-headers to clearly describe changes made here.
    }

    Ok(())
}

fn read_to_body(r: &mut Read, len_estimate: u64, tune: &Tunables)
    -> Result<(BodyImage, u64), FlError>
{
    if len_estimate > tune.max_body_ram {
        let b = BodyImage::with_fs()?;
        return read_to_body_fs(r, b, tune);
    }

    let mut body = BodyImage::with_ram(len_estimate);

    let mut size: u64 = 0;
    'eof: loop {
        let mut buf = BytesMut::with_capacity(tune.decode_buffer_ram);
        'fill: loop {
            let len = match r.read(unsafe { buf.bytes_mut() }) {
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
                break 'fill; // can't break 'eof, because may have len already
            }
            println!("Decoded inner buf len {}", len);
            unsafe { buf.advance_mut(len) };

            if buf.remaining_mut() < 1024 {
                break 'fill;
            }
        }
        let len = buf.len() as u64;
        if len == 0 {
            break 'eof;
        }
        size += len;
        if size > tune.max_body {
            bail!("Decompressed response stream too long: {}+", size);
        }
        if size > tune.max_body_ram {
            body = body.write_back()?;
            println!("Write (Fs) decoded buf len {}", len);
            body.write_all(&buf)?;
            let (b, s) = read_to_body_fs(r, body, tune)?;
            return Ok((b, size + s));
        }
        println!("Saved (Ram) decoded buf len {}", len);
        body.save(buf.freeze().into())?;
    }
    Ok((body, size))
}

fn read_to_body_fs(r: &mut Read, mut body: BodyImage, tune: &Tunables)
    -> Result<(BodyImage, u64), FlError>
{
    let mut size: u64 = 0;
    let mut buf = BytesMut::with_capacity(tune.decode_buffer_fs);
    loop {
        let len = match r.read(unsafe { buf.bytes_mut() }) {
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
        unsafe { buf.advance_mut(len) };

        size += len as u64;
        if size > tune.max_body {
            bail!("Decompressed response stream too long: {}+", size);
        }
        println!("Write (Fs) decoded buf len {}", len);
        body.write_all(&buf)?;
        buf.clear();
    }
    Ok((body, size))
}
