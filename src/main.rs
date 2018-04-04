extern crate body_image;
#[macro_use] extern crate clap;
extern crate failure;
extern crate fern;
extern crate http;
#[macro_use] extern crate log;

use std::process;

use body_image::{VERSION, Tunables};
use body_image::barc::{BarcFile, NoCompressStrategy};
use clap::{Arg, App, SubCommand};
use failure::Error as Flare;

#[cfg(feature = "client")]
mod record;

fn main() {
    let r = run();
    if let Err(e) = r {
        error!("Error cause: {}; (backtrace) {}", e.cause(), e.backtrace());
        process::exit(2);
    }
}

fn setup_app() -> Result<App, Flare> {
    App::new("barc")
        .version(crate_version!())
        .author(crate_authors!("\n"))
        .about("Tool for BARC file recording and manipulation")
}

fn setup_logger() -> Result<(), Flare> {
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} {}: {}",
                record.target(),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .level_for("hyper::proto",  log::LevelFilter::Info)
        .level_for("tokio_core",    log::LevelFilter::Info)
        .level_for("tokio_reactor", log::LevelFilter::Info)
        .chain(std::io::stderr())
        .apply()?;
    Ok(())
}

fn run() -> Result<(), Flare> {
    setup_logger()?;
    Ok(())
}
