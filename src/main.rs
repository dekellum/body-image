             extern crate body_image;
#[macro_use] extern crate clap;
#[macro_use] extern crate failure;
             extern crate fern;
             extern crate http;
#[macro_use] extern crate log;

#[cfg(feature = "client")] mod record;

use std::process;

use clap::{Arg, App, AppSettings, SubCommand};
use failure::Error as Flare;

#[cfg(feature = "client")]
use body_image::barc::{CompressStrategy,
                       NoCompressStrategy,
                       GzipCompressStrategy};

#[cfg(all(feature = "client", feature = "brotli"))]
use body_image::barc::BrotliCompressStrategy;

fn main() {
    let r = run();
    if let Err(e) = r {
        error!("Error cause: {}; (backtrace) {}", e.cause(), e.backtrace());
        process::exit(2);
    }
}

fn run() -> Result<(), Flare> {
    let m = setup_cli().get_matches();

    setup_logger(m.occurrences_of("debug"))?;
    let scname = m.subcommand_name().unwrap(); // required

    match scname {
        "cat" => unimplemented!(),
        #[cfg(feature = "client")]
        "record" => {
            let sm = m.subcommand_matches("record").unwrap();
            let mut cs: Box<CompressStrategy> =
                Box::new(NoCompressStrategy::default());
            match sm.is_present("brotli") {
                #[cfg(feature = "brotli")]
                true => {
                    cs = Box::new(BrotliCompressStrategy::default());
                }
                _ => {}
            }
            if sm.is_present("gzip") {
                cs = Box::new(GzipCompressStrategy::default());
            }
            record::record(sm.value_of("url").unwrap(),
                           sm.value_of("file").unwrap(),
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

fn setup_cli() -> App<'static, 'static> {
    let appl = App::new("barc")
        .version(crate_version!())
        .about("Tool for BARC file recording and manipulation")
        .setting(AppSettings::SubcommandRequired)
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
                .value_name("BARC-FILE")
                .help("path to BARC file to create/append"),
            ]);
    let cat = SubCommand::with_name("cat")
        .about("Print BARC records to standard output");
    appl.subcommand(rec)
        .subcommand(cat)
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
