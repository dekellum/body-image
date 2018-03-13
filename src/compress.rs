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
use super::{BodyImage, Dialog, META_RES_DECODED, Tunables};

/// Decode any gzip or deflate Transfer-Encoding or Content-Encoding
/// into a new `BodyItem`, and update `Dialog` accordingly.
pub fn decode_body(dialog: &mut Dialog, tune: &Tunables) -> Result<(), FlError> {
    let headers = &dialog.res_headers;
    let encodings = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .iter()
        .chain(headers
               .get_all(http::header::CONTENT_ENCODING)
               .iter());

    let mut chunked = false;
    let mut compress = None;

    'headers: for v in encodings {
        // Hyper's Content-Encoding includes Brotli (br) _and_
        // Chunked, is thus a super-set of Transfer-Encoding, so parse
        // all of these headers that way.
        if let Ok(v) = ContentEncoding::parse_header(&Raw::from(v.as_bytes())) {
            for av in v.iter() {
                match *av {
                    Encoding::Chunked => chunked = true,
                    Encoding::Gzip | Encoding::Deflate => { // supported
                        compress = Some(av.clone()); // FIXME: sad clone
                        break 'headers;
                    }
                    Encoding::Identity => (),
                    _ => {
                        println!("Unsupported Encoding for decode: {:?}", av);
                        break 'headers;
                    }
                }
            }
        }
    }

    if let Some(ref comp) = compress {
        let new_body = {
            println!("Body to {:?} decode: {:?}", comp, dialog.body);
            let mut reader = dialog.body.reader();
            match *comp {
                Encoding::Gzip => {
                    let mut decoder = GzDecoder::new(reader.as_read());
                    let len_est = dialog.body.len() *
                        u64::from(tune.gzip_size_x_est);
                    read_to_body(&mut decoder, len_est, tune)?
                }
                Encoding::Deflate => {
                    let mut decoder = DeflateDecoder::new(reader.as_read());
                    let len_est = dialog.body.len() *
                        u64::from(tune.deflate_size_x_est);
                    read_to_body(&mut decoder, len_est, tune)?
                }
                _ => unreachable!("Not supported: {:?}", comp)
            }
        };
        dialog.body = new_body.prepare()?;
        println!("Body update: {:?}", dialog.body);
    }

    if chunked || compress.is_some() {
        let mut ds = Vec::with_capacity(2);
        if chunked {
            ds.push(Encoding::Chunked.to_string())
        }
        if let Some(ref e) = compress {
            ds.push(e.to_string())
        }
        dialog.meta.append(http::header::HeaderName
                           ::from_lowercase(META_RES_DECODED).unwrap(),
                           ds.join(", ").parse()?);
    }
    Ok(())
}

fn read_to_body(r: &mut Read, len_estimate: u64, tune: &Tunables)
    -> Result<BodyImage, FlError>
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
            body.write_back()?;
            println!("Write (Fs) decoded buf len {}", len);
            body.write_all(&buf)?;
            return read_to_body_fs(r, body, tune)
        }
        println!("Saved (Ram) decoded buf len {}", len);
        body.save(buf.freeze().into())?;
    }
    Ok(body)
}

fn read_to_body_fs(r: &mut Read, mut body: BodyImage, tune: &Tunables)
    -> Result<BodyImage, FlError>
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
    Ok(body)
}
