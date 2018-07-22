extern crate fern;

use std;
use log;
use failure::Error as Flare;

// Use lazy static to ensure we only setup logging once (by first test and
// thread)
#[cfg(test)]
lazy_static! {
    pub static ref LOG_SETUP: bool = debug_logger();
}

#[cfg(test)]
pub fn debug_logger() -> bool {
    let level = if let Ok(l) = std::env::var("TEST_LOG") {
        l.parse().expect("TEST_LOG parse integer")
    } else {
        0
    };
    if level > 0 {
        setup_logger(level-1).expect("fern setup logger");
    }
    true
}

pub fn setup_logger(level: u32) -> Result<(), Flare> {
    let mut disp = fern::Dispatch::new()
        .format(|out, message, record| {
            let t = std::thread::current();
            out.finish(format_args!(
                "{} {} {}: {}",
                record.level(),
                record.target(),
                t.name().map(str::to_owned)
                    .unwrap_or_else(|| format!("{:?}", t.id())),
                message
            ))
        });
    disp = if level == 0 {
        disp.level(log::LevelFilter::Info)
    } else {
        disp.level(log::LevelFilter::Debug)
    };

    #[cfg(feature = "async")]
    {
        if level < 2 {
            // These are only for record/client deps
            disp = disp
                .level_for("hyper::proto",  log::LevelFilter::Info)
                .level_for("tokio_core",    log::LevelFilter::Info)
                .level_for("tokio_reactor", log::LevelFilter::Info);
        }
    }

    disp.chain(std::io::stderr())
        .apply()
        .map_err(Flare::from)
}

#[cfg(test)]
mod tests {
    use super::LOG_SETUP;

    /// Sanity test and silence non-use warnings.
    #[test]
    fn log_setup() {
        assert!(*LOG_SETUP);
    }
}
