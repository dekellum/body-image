use std::time::Duration;

use blocking_permit::Semaphore;
use body_image::Tunables;

/// An additional set of tuning constants for asynchronous I/O, extending the
/// [`body_image::Tunables`] set.
///
/// Setters are available via [`FutioTuner`] (a builder type).
#[derive(Debug, Clone)]
pub struct FutioTunables {
    image: Tunables,
    res_timeout:     Option<Duration>,
    body_timeout:    Option<Duration>,
    blocking_policy: BlockingPolicy,
    stream_item_size: usize,
}

/// The policy for blocking operations.
#[derive(Debug, Copy, Clone)]
pub enum BlockingPolicy {
    /// Always run blocking operations directly and without further
    /// coordination.
    Direct,

    /// Acquire a `BlockingPermit` from the referenced `Semaphore` and use this
    /// to run the blocking operation on the _current_ thread.
    Permit(&'static Semaphore),

    /// Dispatch blocking operations to the `DispatchPool` registered on the
    /// current thread.
    Dispatch,
}

impl FutioTunables {
    /// Construct with default values.
    pub fn new() -> FutioTunables {
        FutioTunables {
            image: Tunables::new(),
            res_timeout:  None,
            body_timeout: Some(Duration::from_secs(60)),
            blocking_policy: BlockingPolicy::Direct,
            stream_item_size: 512 * 1024,
        }
    }

    /// Return the base (body-image) `Tunables`.
    ///
    /// Default: As per body-image crate defaults.
    pub fn image(&self) -> &Tunables {
        &self.image
    }

    /// Return the maximum stream item buffer size in bytes, when using
    /// `SplitBodyImage`. Default: 512 KiB.
    pub fn stream_item_size(&self) -> usize {
        self.stream_item_size
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

    /// Return a `Semaphore` reference for use in constraining the number of
    /// concurrent blocking operations.
    ///
    /// Default: None
    pub fn blocking_semaphore(&self) -> Option<&'static Semaphore> {
        if let BlockingPolicy::Permit(sema) = self.blocking_policy {
            Some(sema)
        } else {
            None
        }
    }

    /// Return the policy for any required blocking operations.
    ///
    /// Default: `BlockingPolicy::Direct`
    pub fn blocking_policy(&self) -> BlockingPolicy {
        self.blocking_policy
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

/// A builder for [`FutioTunables`].
///
/// Invariants are asserted in the various setters and `finish`.
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

    /// Set the maximum stream item buffer size in bytes, when using
    /// `SplitBodyImage`.
    pub fn set_stream_item_size(&mut self, size: usize) -> &mut FutioTuner {
        assert!(size > 0, "stream_item_size must be greater than zero");
        self.template.stream_item_size = size;
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

    /// Set policy for blocking. Note that below the highest level interfaces
    /// such as `request_dialog` and `fetch`, setting this should be combined
    /// with using the appropriate `Stream` or `Sink` types, e.g. using
    /// `PermitBodyStream` with `BlockingPolicy::Permit`.
    pub fn set_blocking_policy(&mut self, policy: BlockingPolicy)
        -> &mut FutioTuner
    {
        self.template.blocking_policy = policy;
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
