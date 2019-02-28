use tao_log::debug;

use body_image::{BodyError, BodySink, Tunables};

use hyper;
use tokio_threadpool;
use futures::{Async, AsyncSink, Poll, Sink, StartSend};

use crate::FutioError;

/// Adaptor for `BodySink` implementing the `futures::Sink` trait.  This
/// allows a `hyper::Body` (`hyper::Chunk` item) stream to be forwarded
/// (e.g. via `futures::Stream::forward`) to a `BodySink`, in a fully
/// asynchronous fashion.
///
/// `Tunables` are used during the streaming to decide when to write back a
/// BodySink in `Ram` to `FsWrite`.  This implementation uses
/// `tokio_threadpool::blocking` to request becoming a backup thread for
/// blocking operations including `BodySink::write_back` and
/// `BodySink::write_all` (state `FsWrite`). It may thus only be used on the
/// tokio threadpool. If the `max_blocking` number of backup threads is
/// reached, and a blocking operation is required, then this implementation
/// will appear *full*, with `start_send` returning
/// `Ok(AsyncSink::NotReady(chunk)`, until a backup thread becomes available
/// or any timeout occurs.
#[derive(Debug)]
pub struct AsyncBodySink {
    body: BodySink,
    tune: Tunables,
}

impl AsyncBodySink {
    /// Wrap by consuming a `BodySink` and `Tunables` instances.
    ///
    /// *Note*: Both `BodyImage` and `Tunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    pub fn new(body: BodySink, tune: Tunables) -> AsyncBodySink {
        AsyncBodySink { body, tune }
    }

    /// The inner `BodySink` as constructed.
    pub fn body(&self) -> &BodySink {
        &self.body
    }

    /// A mutable reference to the inner `BodySink`.
    pub fn body_mut(&mut self) -> &mut BodySink {
        &mut self.body
    }

    /// Unwrap and return the `BodySink`.
    pub fn into_inner(self) -> BodySink {
        self.body
    }
}

macro_rules! unblock {
    ($c:ident, || $b:block) => (match tokio_threadpool::blocking(|| $b) {
        Ok(Async::Ready(Ok(_))) => (),
        Ok(Async::Ready(Err(e))) => return Err(e.into()),
        Ok(Async::NotReady) => {
            debug!("No blocking backup thread available -> NotReady");
            return Ok(AsyncSink::NotReady($c));
        }
        Err(e) => return Err(FutioError::Other(Box::new(e)))
    })
}

impl Sink for AsyncBodySink {
    type SinkItem = hyper::Chunk;
    type SinkError = FutioError;

    fn start_send(&mut self, chunk: hyper::Chunk)
        -> StartSend<hyper::Chunk, FutioError>
    {
        let new_len = self.body.len() + (chunk.len() as u64);
        if new_len > self.tune.max_body() {
            return Err(BodyError::BodyTooLong(new_len).into());
        }
        if self.body.is_ram() && new_len > self.tune.max_body_ram() {
            unblock!(chunk, || {
                debug!("to write back file (blocking, len: {})", new_len);
                self.body.write_back(self.tune.temp_dir())
            })
        }
        if self.body.is_ram() {
            debug!("to save chunk (len: {})", chunk.len());
            self.body.save(chunk).map_err(FutioError::from)?;
        } else {
            unblock!(chunk, || {
                debug!("to write chunk (blocking, len: {})", chunk.len());
                self.body.write_all(&chunk)
            })
        }

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), FutioError> {
        Ok(Async::Ready(()))
    }

    fn close(&mut self) -> Poll<(), FutioError> {
        Ok(Async::Ready(()))
    }
}
