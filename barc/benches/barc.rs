//! This was copied from src/barc.rs unit tests with minimal changes and only
//! serves as a rough compression performance sanity check.
#![warn(rust_2018_idioms)]

#![feature(test)]
extern crate test; // Still required, see rust-lang/rust#55133

use std::fs;
use std::path::{Path, PathBuf};

use test::Bencher;

use body_image::{BodySink, Tunables};
use barc::*;

#[bench]
fn write_read_large_plain(b: &mut Bencher) {
    b.iter(|| {
        let fname = barc_test_file("large.barc").unwrap();
        let strategy = NoCompressStrategy::default();
        write_read_large(&fname, &strategy).unwrap();
    })
}

#[bench]
fn write_read_large_gzip(b: &mut Bencher) {
    b.iter(|| {
        let fname = barc_test_file("large_gzip.barc").unwrap();
        let strategy = GzipCompressStrategy::default();
        write_read_large(&fname, &strategy).unwrap();
    })
}

#[bench]
fn write_read_large_gzip_0(b: &mut Bencher) {
    b.iter(|| {
        let fname = barc_test_file("large_gzip_0.barc").unwrap();
        let strategy = GzipCompressStrategy::default().set_compression_level(0);
        write_read_large(&fname, &strategy).unwrap();
    })
}

#[cfg(feature = "brotli")]
#[bench]
fn write_read_large_brotli(b: &mut Bencher) {
    b.iter(|| {
        let fname = barc_test_file("large_brotli.barc").unwrap();
        let strategy = BrotliCompressStrategy::default();
        write_read_large(&fname, &strategy).unwrap();
    })
}

#[cfg(feature = "brotli")]
#[bench]
fn write_read_large_brotli_0(b: &mut Bencher) {
    b.iter(|| {
        let fname = barc_test_file("large_brotli_0.barc").unwrap();
        let strategy = BrotliCompressStrategy::default()
            .set_compression_level(0);
        write_read_large(&fname, &strategy).unwrap();
    })
}

fn write_read_large(fname: &PathBuf, strategy: &dyn CompressStrategy)
    -> Result<(), Flaw>
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

    let req_reps =   500;
    let res_reps = 1_000;

    let mut req_body = BodySink::with_ram_buffers(req_reps);
    for _ in 0..req_reps {
        req_body.push(lorem_ipsum)?;
    }
    let req_body = req_body.prepare()?;

    let mut res_body = BodySink::with_ram_buffers(res_reps);
    for _ in 0..res_reps {
        res_body.push(lorem_ipsum)?;
    }
    let res_body = res_body.prepare()?;

    let mut res_headers = http::HeaderMap::default();
    res_headers.insert(http::header::CONTENT_TYPE, "text/plain".parse()?);
    let req_headers = res_headers.clone();
    let mut meta = http::HeaderMap::default();
    meta.insert(hname_meta_res_decoded(), "identity".parse()?);

    writer.write(
        &Record { req_body, req_headers, res_body, res_headers, meta,
                  ..Record::default()},
        strategy)?;

    let tune = Tunables::new();
    let mut reader = bfile.reader()?;
    let record = reader.read(&tune)?.unwrap();

    assert_eq!(record.rec_type, RecordType::Dialog);

    Ok(())
}

fn barc_test_file(name: &str) -> Result<PathBuf, Flaw> {
    let target = env!("CARGO_MANIFEST_DIR");
    let path = format!("{}/../target/testmp", target);
    let tpath = Path::new(&path);
    fs::create_dir_all(tpath)?;

    let fname = tpath.join(name);
    if fname.exists() {
        fs::remove_file(&fname)?;
    }
    Ok(fname)
}
