use std::ops::Deref;

#[cfg(unix)] use log::debug;
use olio::mem::{MemAdvice, MemAdviseError, MemHandle};

use crate::BodyError;

// Extension trait for temporary madvise, here mostly for logging purposes
pub trait MemHandleExt {
    fn tmp_advise<F, R, S>(&self, advice: MemAdvice, f: F) -> Result<R, S>
        where F: FnOnce() -> Result<R, S>,
              S: From<olio::mem::MemAdviseError>;
}

impl<T> MemHandleExt for MemHandle<T>
    where T: Deref<Target=[u8]>
{
    fn tmp_advise<F, R, S>(&self, advice: MemAdvice, f: F) -> Result<R, S>
        where F: FnOnce() -> Result<R, S>,
              S: From<MemAdviseError>
    {
        let _new_advice = self.advise(advice)?;

        #[cfg(unix)]
        {
            debug!("MemHandle tmp_advise {:?}, obtained {:?}",
                   advice, _new_advice);
        }

        let res = f();
        self.advise(MemAdvice::Normal)?;
        res
    }
}

impl From<MemAdviseError> for BodyError {
    fn from(err: MemAdviseError) -> BodyError {
        BodyError::Io(err.into())
    }
}
