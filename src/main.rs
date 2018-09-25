//! The `barc` command line tool
#![deny(dead_code, unused_imports)]
#![warn(bare_trait_objects)]

use std::io;
use std::process;

use clap::{
    Arg, ArgMatches, App, AppSettings,
    crate_version, SubCommand
};
use failure::{bail, Error as Flare};
use log::error;

use body_image::{Tunables, RequestRecorded, Recorded};
use body_image::barc::{
    BarcFile,
    CompressStrategy, GzipCompressStrategy, NoCompressStrategy,
    write_body, write_headers, MetaRecorded
};
#[cfg(feature = "brotli")] use body_image::barc::BrotliCompressStrategy;

use crate::logger::setup_logger;

#[cfg(feature = "futio")] mod record;
mod logger;

fn main() {
    let r = run();
    if let Err(e) = r {
        error!("{}; (backtrace) {}", e.as_fail(), e.backtrace());
        process::exit(2);
    }
}

fn run() -> Result<(), Flare> {
    let var_help = VarHelp::default();
    let m = setup_cli(&var_help).get_matches();

    setup_logger(m.occurrences_of("debug") as u32)?;
    let scname = m.subcommand_name().unwrap(); // required
    let subm = m.subcommand_matches(scname).unwrap();

    match scname {
        "cat" => {
            let parts = part_flags(subm);
            let files = subm.values_of("file").unwrap();
            let (start, count) = filter_flags(subm)?;
            for f in files {
                cat(f, &start, count, &parts)?;
            }
            Ok(())
        }
        "cp" => {
            let cs = compress_flags(subm)?;
            let files = subm.values_of("file").unwrap();
            let fout = subm.value_of("out-file").unwrap();
            let (start, count) = filter_flags(subm)?;
            for fin in files {
                cp(fin, fout, &start, count, cs.as_ref())?;
            }
            Ok(())
        }
        #[cfg(feature = "futio")]
        "record" => {
            let cs = compress_flags(subm)?;
            record::record(subm.value_of("url").unwrap(),
                           subm.value_of("file").unwrap(),
                           !subm.is_present("no_decode"),
                           subm.value_of("accept"),
                           cs.as_ref())
        }
        #[cfg(not(feature = "futio"))]
        "record" => {
            bail!("Sub-command \"record\" requires the \"client\" \
                   build feature");
        }
        _ => {
            bail!("Sub-command \"{}\" not supported", scname);
        }
    }
}

// Parse the field selection flags into `Parts`
fn part_flags(matches: &ArgMatches) -> Parts {
    let mut parts = Parts::default();
    if  matches.is_present("meta") ||
        matches.is_present("req_head") || matches.is_present("req_body") ||
        matches.is_present("res_head") || matches.is_present("res_body")
    {
        parts = Parts::falsie();
        if matches.is_present("meta")     { parts.meta     = true; }
        if matches.is_present("req_head") { parts.req_head = true; }
        if matches.is_present("req_body") { parts.req_body = true; }
        if matches.is_present("res_head") { parts.res_head = true; }
        if matches.is_present("res_body") { parts.res_body = true; }
    }
    parts
}

// Parse the compress flags into a CompressStrategy
fn compress_flags(matches: &ArgMatches) -> Result<Box<dyn CompressStrategy>, Flare>
{
    let mut cs: Box<dyn CompressStrategy> = Box::new(NoCompressStrategy::default());
    if matches.is_present("brotli") {
        #[cfg(feature = "brotli")] {
            cs = Box::new(BrotliCompressStrategy::default());
        }
        #[cfg(not(feature = "brotli"))] {
            bail!("Brotli compression requires the \"broli\" build feature");
        }
    }
    // brotli/gzip are exclusive (conflicts_with)
    if matches.is_present("gzip") {
        cs = Box::new(GzipCompressStrategy::default());
    }
    Ok(cs)
}

// Parse filter flags and return (StartPos, count) or error.
fn filter_flags(matches: &ArgMatches) -> Result<(StartPos, usize), Flare>
{
    let files_len = matches.values_of("file").unwrap().len();
    let mut start = StartPos::default();
    let mut count = usize::max_value();
    if let Some(v) = matches.value_of("index") {
        if files_len > 1 {
            bail!("--index flag can't be applied to more than one \
                   input file");
        }
        let v = v.parse()?;
        start = StartPos::Index(v);
    }
    // index/offset are exclusive (conflicts_with)
    if let Some(v) = matches.value_of("offset") {
        if files_len > 1 {
            bail!("--offset flag can't be applied to more than one \
                   input file");
        }
        let v = if v.starts_with("0x") {
            u64::from_str_radix(&v[2..], 16)?
        } else {
            v.parse()?
        };
        start = StartPos::Offset(v);
    }
    if let Some(v) = matches.value_of("count") {
        if files_len > 1 {
            bail!("--count flag can't be applied to more than one \
                   input file");
        }
        count = v.parse()?;
    }
    Ok((start, count))
}

enum StartPos {
    Offset(u64),
    Index(usize)
}
impl Default for StartPos {
    fn default() -> StartPos { StartPos::Index(0) }
}

struct Parts {
    meta: bool,
    req_head: bool,
    req_body: bool,
    res_head: bool,
    res_body: bool,
}

impl Parts {
    fn falsie() -> Parts {
        Parts { meta: false,
                req_head: false, req_body: false,
                res_head: false, res_body: false }
    }
}

impl Default for Parts {
    fn default() -> Parts {
        Parts { meta: true,
                req_head: true, req_body: true,
                res_head: true, res_body: true }
    }
}

// The `cat` command implementation.
fn cat(barc_path: &str, start: &StartPos, count: usize, parts: &Parts)
    -> Result<(), Flare>
{
    let bfile = BarcFile::new(barc_path);
    let tune = Tunables::new();
    let mut reader = bfile.reader()?;
    if let StartPos::Offset(o) = *start {
        reader.seek(o)?;
    }
    let fout = &mut io::stdout();
    let nl = true;
    let mut i = 0;
    let mut offset = reader.offset();
    while let Some(rec) = reader.read(&tune)? {
        if let StartPos::Index(s) = *start {
            if i < s { i += 1; continue; }
        }
        i += 1;
        if i > count { break; }
        println!("====== file {:-<38} offset {:#012x} ======",
                 barc_path, offset);
        if parts.meta     { write_headers(fout, nl, rec.meta())?; }
        if parts.req_head { write_headers(fout, nl, rec.req_headers())?; }
        if parts.req_body { write_body   (fout, nl, rec.req_body())?; }
        if parts.res_head { write_headers(fout, nl, rec.res_headers())?; }
        if parts.res_body { write_body   (fout, nl, rec.res_body())?; }

        offset = reader.offset();
    }

    Ok(())
}

// The `cp` command implementation.
fn cp(
    barc_in: &str,
    barc_out: &str,
    start: &StartPos,
    count: usize,
    strategy: &dyn CompressStrategy)
    -> Result<(), Flare>
{
    if barc_in == barc_out {
        bail!("BARC input and output must be different files");
    }
    let bfin = BarcFile::new(barc_in);
    let tune = Tunables::new();
    let mut reader = bfin.reader()?;
    if let StartPos::Offset(o) = *start {
        reader.seek(o)?;
    }
    let bfout = BarcFile::new(barc_out);
    let mut writer = bfout.writer()?;

    let mut i = 0;
    while let Some(record) = reader.read(&tune)? {
        if let StartPos::Index(s) = *start {
            if i < s { i += 1; continue; }
        }
        i += 1;
        if i > count { break; }
        writer.write(&record, strategy)?;
    }

    Ok(())
}

// Container for dynamically generated help text, to appease Clap's str
// reference help text lifetimes.
struct VarHelp {
    record_about: String,
    record_after: String
}

impl Default for VarHelp {
    fn default() -> VarHelp {
        let feature = if cfg!(feature = "futio") {
            "included"
        } else {
            "not included"
        };
        let record_about = format!(
            "Record an HTTP dialog via the network ({})",
            feature);
        let record_after = format!(
            "This command depends on the non-default \"client\" feature at \
             build time, which was {}.\n\
             \n\
             Currently `record` is limited to GET requests.  The browser-like \
             (HTML preferring) Accept header default can be overridden by the \
             --accept option. The flags (--gzip, --brotli) control output \
             compression. By default, no compression is used.",
            feature);
        VarHelp { record_about, record_after }
    }
}

fn setup_cli<'a, 'b>(var_help: &'a VarHelp) -> App<'a, 'b>
    where 'a: 'b
{
    let rec = SubCommand::with_name("record")
        .setting(AppSettings::DeriveDisplayOrder)
        .about(var_help.record_about.as_str())
        .after_help(var_help.record_after.as_str())
        .args(&[
            Arg::with_name("gzip")
                .short("z")
                .long("gzip")
                .help("Use gzip compression strategy"),
            Arg::with_name("brotli")
                .short("b")
                .long("brotli")
                .conflicts_with("gzip")
                .help("Use Brotli compression strategy (brotli feature)"),
            Arg::with_name("no_decode")
                .long("no-decode")
                .help("Don't attempt to decode any \
                       Content or Transfer-Encoding"),
            Arg::with_name("accept")
                .long("accept")
                .number_of_values(1)
                .help("Set request Accept header value, e.g. \"*/*\""),
            Arg::with_name("url")
                .required(true)
                .index(1)
                .value_name("URL")
                .help("The http(s)://... URL to fetch"),
            Arg::with_name("file")
                .required(true)
                .index(2)
                .value_name("BARC-OUT-FILE")
                .help("Path to BARC file to create/append"),
        ]);

    let cat = SubCommand::with_name("cat")
        .setting(AppSettings::DeriveDisplayOrder)
        .about("Print BARC records to standard output")
        .after_help(
            "By default, prints all fields of all records in the given BARC \
             file(s). Records are decompressed as needed. Instead of the \
             machine readable BARC record head, an informational separator \
             line is printed at the begining of each record, including the \
             file name and offset of the record in hexadecimal. Use the `cp` \
             command instead to output BARC formatted records to a file.\n\
             \n\
             The output flags (--meta, --req-body, etc.) can be used to \
             select specific fields. The options (--offset, --index, --count) \
             can be used to select record(s) from a single input file.")
        .args(&[
            Arg::with_name("offset")
                .short("o")
                .long("offset")
                .number_of_values(1)
                .help("Offset in bytes to first input record \
                       (prefix with \"0x\" for hex)"),
            Arg::with_name("index")
                .short("i")
                .long("index")
                .number_of_values(1)
                .conflicts_with("offset")
                .help("Zero-based index to first input record (default: 0)"),
            Arg::with_name("count")
                .short("c")
                .long("count")
                .number_of_values(1)
                .help("Count of records to print (default: all until EOF)"),
            Arg::with_name("meta")
                .long("meta")
                .alias("meta-head")
                .help("Output meta headers"),
            Arg::with_name("req_head")
                .long("req-head")
                .help("Output request headers"),
            Arg::with_name("req_body")
                .long("req-body")
                .help("Output request bodies"),
            Arg::with_name("res_head")
                .long("res-head")
                .help("Output response headers"),
            Arg::with_name("res_body")
                .long("res-body")
                .help("Output response bodies"),
            Arg::with_name("file")
                .required(true)
                .multiple(true)
                .value_name("BARC-FILE")
                .help("BARC file path(s) from which to read")
        ]);

    let cp = SubCommand::with_name("cp")
        .setting(AppSettings::DeriveDisplayOrder)
        .setting(AppSettings::DontDelimitTrailingValues)
        .about("Copy BARC records from input(s) to an output file")
        .after_help(
            "By default, copies all records from the input file(s) and appends \
             to the output file. The options (--offset, --index, --count) can \
             be used to select record(s) from a single input file. The flags \
             (--gzip, --brotli) control output compression. By default, no \
             compression is used.")
        .args(&[
            Arg::with_name("offset")
                .short("o")
                .long("offset")
                .number_of_values(1)
                .help("Offset in bytes to first input record"),
            Arg::with_name("index")
                .short("i")
                .long("index")
                .number_of_values(1)
                .conflicts_with("offset")
                .help("Zero based index to first input record (default: 0)"),
            Arg::with_name("count")
                .short("c")
                .long("count")
                .number_of_values(1)
                .help("Count of records to print (default: all until EOF)"),
            Arg::with_name("gzip")
                .short("z")
                .long("gzip")
                .help("Use gzip compression strategy"),
            Arg::with_name("brotli")
                .short("b")
                .long("brotli")
                .conflicts_with("gzip")
                .help("Use Brotli compression strategy (brotli feature)"),
            Arg::with_name("file")
                .required(true)
                .multiple(true)
                .value_name("BARC-IN-FILE")
                .help("BARC file path(s) from which to read"),
            Arg::with_name("out-file")
                .required(true)
                .value_name("BARC-OUT-FILE")
                .help("BARC file path to create/append")
        ]);

    App::new("barc")
        .version(crate_version!())
        .about("Tool for BARC file recording and manipulation")
        .setting(AppSettings::SubcommandRequired)
        .setting(AppSettings::DeriveDisplayOrder)
        .max_term_width(100)
        .arg(Arg::with_name("debug")
             .short("d")
             .long("debug")
             .multiple(true)
             .help("Enable more logging, and up to `-ddd`")
             .global(true))
        .subcommand(cat)
        .subcommand(cp)
        .subcommand(rec)
}
