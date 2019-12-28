
/// All possible states of a [`BlockingArbiter`].
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum Blocking {
    // Not requested nor allowed
    Void,
    // Requested Once
    Pending,
    // One blocking operation granted
    Once,
    // Any/all blocking operations perpetually granted
    Always,
}

/// Trait for arbitration of where blocking operations are acceptable to the
/// runtime.
pub trait BlockingArbiter {
    /// Return true if blocking is allowed.
    ///
    /// This may record the need to block and/or consume any one-time blocking
    /// allowance.
    fn can_block(&mut self) -> bool;

    /// Return the current blocking state, without making any change.
    fn state(&self) -> Blocking;
}

/// A lenient arbiter that always allows blocking.
#[derive(Debug, Default)]
pub struct LenientArbiter;

impl BlockingArbiter for LenientArbiter {

    /// Return true if blocking is allowed.
    ///
    /// This implementation always returns true.
    fn can_block(&mut self) -> bool {
        true
    }

    fn state(&self) -> Blocking {
        Blocking::Always
    }
}

/// A stateful arbiter that records the need to block and and grants one-time
/// allowances.
#[derive(Debug)]
pub struct StatefulArbiter {
    state: Blocking
}

impl Default for StatefulArbiter {
    fn default() -> Self {
        StatefulArbiter { state: Blocking::Void }
    }
}

impl StatefulArbiter {
    pub(crate) fn set(&mut self, state: Blocking) {
        self.state = state;
    }
}

impl BlockingArbiter for StatefulArbiter {
    /// Return true if blocking is allowed, consuming any one-time allowance.
    /// Otherwise records that blocking has been requested.
    fn can_block(&mut self) -> bool {
        match self.state {
            Blocking::Void => {
                self.state = Blocking::Pending;
                false
            }
            Blocking::Pending => {
                false
            }
            Blocking::Once => {
                self.state = Blocking::Void;
                true
            }
            Blocking::Always => {
                true
            }
        }
    }

    fn state(&self) -> Blocking {
        self.state
    }
}
