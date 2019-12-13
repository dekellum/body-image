use std::time::Duration;

use blocking_permit::Semaphore;
use body_image::Tunables;

/// An additional set of tuning constants for futures I/O, extending the
/// body-image `Tunables` set by composition.  Setters are available via
/// [`FutioTuner`](struct.FutioTuner.html) (a builder type).
#[derive(Debug, Clone)]
pub struct FutioTunables {
    image: Tunables,
    res_timeout:     Option<Duration>,
    body_timeout:    Option<Duration>,
    blocking_semaphore: Option<&'static Semaphore>,
}

impl FutioTunables {
    /// Construct with default values.
    pub fn new() -> FutioTunables {
        FutioTunables {
            image: Tunables::new(),
            res_timeout:  None,
            body_timeout: Some(Duration::from_secs(60)),
            blocking_semaphore: None,
        }
    }

    /// Return the base (body-image) `Tunables`.
    ///
    /// Default: As per body-image crate defaults.
    pub fn image(&self) -> &Tunables {
        &self.image
    }

    /// Return the maximum initial response timeout interval.
    /// Default: None (e.g. unset)
    pub fn res_timeout(&self) -> Option<Duration> {
        self.res_timeout
    }

    /// Return the maximum streaming body timeout interval.
    /// Default: 60 seconds
    pub fn body_timeout(&self) -> Option<Duration> {
        self.body_timeout
    }

    /// Return a Sempahore reference for use in constraining the number of
    /// concurrent blocking operations.
    ///
    /// Default: None
    pub fn blocking_semaphore(&self) -> Option<&'static Semaphore> {
        self.blocking_semaphore
    }
}

impl Default for FutioTunables {
    fn default() -> Self { FutioTunables::new() }
}

impl AsRef<Tunables> for FutioTunables {
    fn as_ref(&self) -> &Tunables {
        self.image()
    }
}

pub struct FutioTuner {
    template: FutioTunables
}

impl FutioTuner {
    /// Construct with defaults.
    pub fn new() -> FutioTuner {
        FutioTuner { template: FutioTunables::new() }
    }

    /// Set the base body-image `Tunables`.
    pub fn set_image(&mut self, image: Tunables) -> &mut FutioTuner {
        self.template.image = image;
        self
    }

    /// Set the maximum initial response timeout interval.
    pub fn set_res_timeout(&mut self, dur: Duration) -> &mut FutioTuner {
        self.template.res_timeout = Some(dur);
        self
    }

    /// Unset (e.g. disable) response timeout
    pub fn unset_res_timeout(&mut self) -> &mut FutioTuner {
        self.template.res_timeout = None;
        self
    }

    /// Set the maximum streaming body timeout interval.
    pub fn set_body_timeout(&mut self, dur: Duration) -> &mut FutioTuner {
        self.template.body_timeout = Some(dur);
        self
    }

    /// Unset (e.g. disable) body timeout
    pub fn unset_body_timeout(&mut self) -> &mut FutioTuner {
        self.template.body_timeout = None;
        self
    }

    /// Set `Sempahore reference for use in constraining the number of
    /// concurrent blocking operations.
    pub fn set_blocking_semaphore(&mut self, semaphore: &'static Semaphore)
        -> &mut FutioTuner
    {
        self.template.blocking_semaphore = Some(semaphore);
        self
    }

    /// Finish building, asserting any remaining invariants, and return a new
    /// `FutioTunables` instance.
    pub fn finish(&self) -> FutioTunables {
        let t = self.template.clone();
        if t.res_timeout.is_some() && t.body_timeout.is_some() {
            assert!(t.res_timeout.unwrap() <= t.body_timeout.unwrap(),
                    "res_timeout can't be greater than body_timeout");
        }
        t
    }

}

impl Default for FutioTuner {
    fn default() -> Self { FutioTuner::new() }
}
