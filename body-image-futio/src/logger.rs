use std;
use std::sync::Once;

use fern;
use tao_log::log;

use crate::Flaw;

pub fn test_logger() -> bool {
    static TEST_LOG_INIT: Once = Once::new();

    TEST_LOG_INIT.call_once(|| {
        let level = if let Ok(l) = std::env::var("TEST_LOG") {
            l.parse().expect("TEST_LOG parse integer")
        } else {
            0
        };
        if level > 0 {
            setup_logger(level-1).expect("fern setup logger");
        }
    });
    true
}

fn setup_logger(level: u32) -> Result<(), Flaw> {
    let mut disp = fern::Dispatch::new()
        .format(|out, message, record| {
            let t = std::thread::current();
            let tn = t.name().unwrap_or("-");
            out.finish(format_args!(
                "{} {} {}: {}",
                record.level(), record.target(), tn, message
            ));
        });
    disp = if level == 0 {
        disp.level(log::LevelFilter::Info)
    } else if level < 3 {
        disp.level(log::LevelFilter::Debug)
    } else {
        disp.level(log::LevelFilter::Trace)
    };
    if level < 2 {
        disp = disp
            .level_for("hyper::proto",   log::LevelFilter::Info)
            .level_for("tokio_net",      log::LevelFilter::Info)
            .level_for("tokio_executor", log::LevelFilter::Info);
    }

    disp.chain(std::io::stderr())
        .apply()
        .map_err(Flaw::from)
}

mod tests {
    use super::test_logger;

    /// Sanity test and silence non-use warnings.
    #[test]
    fn log_setup() {
        assert!(test_logger());
    }
}
