             extern crate body_image;
#[macro_use] extern crate clap;
#[macro_use] extern crate failure;
             extern crate fern;
             extern crate http;
#[macro_use] extern crate log;

#[cfg(feature = "client")] mod record;

use std::io;
use std::process;
use body_image::{Tunables, RequestRecorded, Recorded};
use body_image::barc::{BarcFile, write_body, write_headers};
use clap::{Arg, ArgMatches, App, AppSettings, SubCommand};
use failure::Error as Flare;

use body_image::barc::{CompressStrategy,
                       NoCompressStrategy,
                       GzipCompressStrategy};

#[cfg(feature = "brotli")]
use body_image::barc::BrotliCompressStrategy;

fn main() {
    let r = run();
    if let Err(e) = r {
        error!("{}; (backtrace) {}", e.cause(), e.backtrace());
        process::exit(2);
    }
}

fn run() -> Result<(), Flare> {
    let m = setup_cli().get_matches();

    setup_logger(m.occurrences_of("debug"))?;
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
            let mut files = subm.values_of("file").unwrap();
            let fout = subm.value_of("out-file").unwrap();
            let (start, count) = filter_flags(subm)?;
            for fin in files {
                cp(fin, fout, &start, count, cs.as_ref())?;
            }
            Ok(())
        }
        #[cfg(feature = "client")]
        "record" => {
            let cs = compress_flags(subm)?;
            record::record(subm.value_of("url").unwrap(),
                           subm.value_of("file").unwrap(),
                           cs.as_ref())
        }
        #[cfg(not(feature = "client"))]
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
        matches.is_present("req_headers") || matches.is_present("req_body") ||
        matches.is_present("res_headers") || matches.is_present("res_body")
    {
        parts = Parts::falsie();
        if matches.is_present("meta")        { parts.meta        = true; }
        if matches.is_present("req_headers") { parts.req_headers = true; }
        if matches.is_present("req_body")    { parts.req_body    = true; }
        if matches.is_present("res_headers") { parts.res_headers = true; }
        if matches.is_present("res_body")    { parts.res_body    = true; }
    }
    parts
}

// Parse the compress flags into a CompressStrategy
fn compress_flags(matches: &ArgMatches) -> Result<Box<CompressStrategy>, Flare>
{
    let mut cs: Box<CompressStrategy> = Box::new(NoCompressStrategy::default());
    if matches.is_present("brotli") {
        if cfg!(feature = "brotli") {
            cs = Box::new(BrotliCompressStrategy::default());
        } else {
            bail!("Brotli compression requires the \"broli\" build feature");
        }
    }
    if matches.is_present("gzip") {
        cs = Box::new(GzipCompressStrategy::default());
    }
    Ok(cs)
}

// Parse filter flags into a (StartPos, count) tupple, or error.
fn filter_flags(matches: &ArgMatches) -> Result<(StartPos, usize), Flare> {
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
    if let Some(v) = matches.value_of("offset") {
        if files_len > 1 {
            bail!("--offset flag can't be applied to more than one \
                   input file");
        }
        let v = v.parse()?;
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
    req_headers: bool,
    req_body: bool,
    res_headers: bool,
    res_body: bool,
}

impl Parts {
    fn falsie() -> Parts {
        Parts { meta: false,
                req_headers: false, req_body: false,
                res_headers: false, res_body: false }
    }
}

impl Default for Parts {
    fn default() -> Parts {
        Parts { meta: true,
                req_headers: true, req_body: true,
                res_headers: true, res_body: true }
    }
}

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
    let mut i = 0;
    let mut offset = reader.offset();
    while let Some(record) = reader.read(&tune)? {
        if let StartPos::Index(s) = *start {
            if i < s { i += 1; continue; }
        }
        i += 1;
        if i > count { break; }
        println!("====== file {:-<38} offset {:#012x} ======",
                 barc_path, offset);
        if parts.meta        { write_headers(fout, true, record.meta())?; }
        if parts.req_headers { write_headers(fout, true, record.req_headers())?; }
        if parts.req_body    { write_body   (fout, true, record.req_body())?; }
        if parts.res_headers { write_headers(fout, true, record.res_headers())?; }
        if parts.res_body    { write_body   (fout, true, record.res_body())?; }

        offset = reader.offset();
    }

    Ok(())
}

fn cp(barc_in: &str,
      barc_out: &str,
      start: &StartPos,
      count: usize,
      strategy: &CompressStrategy)
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

fn setup_cli() -> App<'static, 'static> {
    let appl = App::new("barc")
        .version(crate_version!())
        .about("Tool for BARC file recording and manipulation")
        .setting(AppSettings::SubcommandRequired)
        .setting(AppSettings::DeriveDisplayOrder)
        .arg(Arg::with_name("debug")
             .short("d")
             .long("debug")
             .multiple(true)
             .help("enable additional logging (may repeat for more)")
             .global(true) );

    let rec_about = if cfg!(feature = "client") {
        "Record an HTTP dialog via network (included)"
    } else {
        "Record an HTTP dialog via network \
         (not built, needs \"client\" feature)"
    };
    let rec = SubCommand::with_name("record")
        .setting(AppSettings::DeriveDisplayOrder)
        .about(rec_about)
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
            Arg::with_name("url")
                .required(true)
                .index(1)
                .value_name("URL")
                .help("the (http://) URL to fetch"),
            Arg::with_name("file")
                .required(true)
                .index(2)
                .value_name("BARC-OUT-FILE")
                .help("path to BARC file to create/append"),
        ]);

    let cat = SubCommand::with_name("cat")
        .setting(AppSettings::DeriveDisplayOrder)
        .about("Print BARC records to standard output")
        .after_help(
            "By default prints all records and all fields within each record \
             from the given BARC File(s). The output flags \
             (--meta, --req-body, etc.) can be used to select specific \
             fields. The flags (--offset, --index, --count) can be used to \
             filter a single input file.")
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
                .conflicts_with("start-offset")
                .help("Zero based index to first input record (Default: 0)"),
            Arg::with_name("count")
                .short("c")
                .long("count")
                .number_of_values(1)
                .help("Count of records to print (Default: all until EOF)"),
            Arg::with_name("meta")
                .long("meta")
                .alias("meta-head")
                .help("Output meta headers"),
            Arg::with_name("req_headers")
                .long("req-head")
                .help("Output request headers"),
            Arg::with_name("req_body")
                .long("req-body")
                .help("Output request bodies"),
            Arg::with_name("res_headers")
                .long("res-head")
                .help("Output response headers"),
            Arg::with_name("res_body")
                .long("res-body")
                .help("Output response bodies"),
            Arg::with_name("file")
                .required(true)
                .multiple(true)
                .value_name("BARC-FILE")
                .help("BARC file paths from which to read")
        ]);

    let cp = SubCommand::with_name("cp")
        .setting(AppSettings::DeriveDisplayOrder)
        .setting(AppSettings::DontDelimitTrailingValues)
        .about("Copy BARC records from input(s) to an output file")
        .after_help(
            "By default copies all records from input file(s) to the output \
             file. The flags (--offset, --index, --count) can be used to \
             filter a single input file.")
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
                .conflicts_with("start-offset")
                .help("Zero based index to first input record (Default: 0)"),
            Arg::with_name("count")
                .short("c")
                .long("count")
                .number_of_values(1)
                .help("Count of records to print (Default: all until EOF)"),
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
                .help("BARC file paths from which to read and final to write"),
            Arg::with_name("out-file")
                .required(true)
                .value_name("BARC-OUT-FILE")
                .help("BARC file path to create/append")
        ]);

    appl.subcommand(cat)
        .subcommand(cp)
        .subcommand(rec)
}

fn setup_logger(debug_flags: u64) -> Result<(), Flare> {
    let mut disp = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {}: {}",
                record.target(),
                record.level(),
                message
            ))
        });
    disp = if debug_flags == 0 {
        disp.level(log::LevelFilter::Info)
    } else {
        disp.level(log::LevelFilter::Debug)
    };

    disp = if debug_flags < 2 {
        // These are only for record/client deps, but are harmless if not
        // loaded.
        disp.level_for("hyper::proto",  log::LevelFilter::Info)
            .level_for("tokio_core",    log::LevelFilter::Info)
            .level_for("tokio_reactor", log::LevelFilter::Info)
    } else {
        disp
    };

    disp.chain(std::io::stderr())
        .apply()
        .map_err(Flare::from)
}
