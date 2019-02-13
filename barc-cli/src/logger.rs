use std;

use fern;
use log;

use crate::Flaw;

pub fn setup_logger(level: u32) -> Result<(), Flaw> {
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

    #[cfg(feature = "futio")]
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
        .map_err(Flaw::from)
}
