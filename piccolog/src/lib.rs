//! Provides a very simple Log output implementation for testing and CLI.

use std::cell::RefCell;
use std::error::Error as StdError;
use std::io::Write;
use std::rc::Rc;
use std::sync::Once;
use std::thread_local;

type Flaw = Box<dyn StdError + Send + Sync + 'static>;

struct Piccolog {
    filter: bool
}

impl Piccolog {
    fn filter(&self, meta: &log::Metadata<'_>) -> bool {
        !( self.filter &&
           !meta.target().starts_with("body_image") &&
           !meta.target().starts_with("barc") &&
           !meta.target().starts_with("blocking_permit") &&
           meta.level() > log::Level::Info )
    }

    fn thread_name(&self) -> Rc<String> {
        thread_local! {
            pub static TNAME: RefCell<Option<Rc<String>>> = RefCell::new(None);
        }

        let tn = TNAME.with(|c| {
            if let Some(n) = c.borrow().as_ref() {
                Some(n.clone())
            } else {
                None
            }
        });

        if let Some(n) = tn {
            return n;
        }

        TNAME.with(|c| {
            let t = std::thread::current();
            let mut tn = t.name().unwrap_or("-").to_owned();
            if tn == "tokio-runtime-worker" {
                tn = format!("tokio-w-{:?}", t.id());
            }
            *c.borrow_mut() = Some(Rc::new(tn));
            c.borrow().as_ref().unwrap().clone()
        })
    }
}

impl log::Log for Piccolog {
    fn enabled(&self, meta: &log::Metadata<'_>) -> bool {
        self.filter(meta)
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.filter(record.metadata()) {
            let tn = self.thread_name();
            let _ = writeln!(
                std::io::stderr(),
                "{:5} {} {}: {}",
                record.level(), record.target(), tn, record.args()
            );
        }
    }
    fn flush(&self) {
        std::io::stderr().flush().ok();
    }
}

/// Setup logger for a test run, if not already setup, based on TEST_LOG
/// environment variable.
pub fn test_logger() -> bool {
    static TEST_LOG_INIT: Once = Once::new();

    TEST_LOG_INIT.call_once(|| {
        let level = if let Ok(l) = std::env::var("TEST_LOG") {
            l.parse().expect("TEST_LOG parse integer")
        } else {
            0
        };
        if level > 0 {
            setup_logger(level-1).expect("setup logger");
        }
    });
    true
}

/// Setup logger based on specified level.
///
/// Will fail if already setup.
pub fn setup_logger(level: u32) -> Result<(), Flaw> {
    if level == 0 {
        log::set_max_level(log::LevelFilter::Info)
    } else if level < 3 {
        log::set_max_level(log::LevelFilter::Debug)
    } else {
        log::set_max_level(log::LevelFilter::Trace)
    }

    let filter = level < 2;
    log::set_boxed_logger(Box::new(Piccolog { filter }))?;
    Ok(())
}

#[cfg(test)]mod tests {
    use super::test_logger;
    use log::debug;

    #[test]
    fn log_setup() {
        assert!(test_logger());
        debug!("log message");
        debug!("log message 2");
    }
}
