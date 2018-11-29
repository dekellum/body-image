use bytes::Buf;
use failure::{bail, Error as Flare};
use futures::{Async, AsyncSink, Poll, Sink, StartSend};
use log::debug;
use tokio_threadpool;

use body_image::{BodySink, Tunables};

use crate::UniBodyBuf;

/// Adaptor for `BodySink` implementing the `futures::Sink` trait.  This
/// allows a `Stream<Item=UniBodyBuf>` to be forwarded (e.g. via
/// `futures::Stream::forward`) to a `BodySink`, in a fully asynchronous
/// fashion and with zero-copy `MemMap` support (*mmap* feature only).
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
pub struct UniBodySink {
    body: BodySink,
    tune: Tunables,
}

impl UniBodySink {
    /// Wrap by consuming a `BodySink` and `Tunables` instances.
    ///
    /// *Note*: Both `BodyImage` and `Tunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    pub fn new(body: BodySink, tune: Tunables) -> UniBodySink {
        UniBodySink { body, tune }
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
        Err(e) => return Err(e.into())
    })
}

impl Sink for UniBodySink {
    type SinkItem = UniBodyBuf;
    type SinkError = Flare;

    fn start_send(&mut self, buf: UniBodyBuf)
        -> StartSend<UniBodyBuf, Flare>
    {
        let new_len = self.body.len() + (buf.remaining() as u64);
        if new_len > self.tune.max_body() {
            bail!("Response stream too long: {}+", new_len);
        }
        if self.body.is_ram() && new_len > self.tune.max_body_ram() {
            unblock!(buf, || {
                debug!("to write back file (blocking, len: {})", new_len);
                self.body.write_back(self.tune.temp_dir())
            })
        }
        if self.body.is_ram() {
            debug!("to save buf (len: {})", buf.remaining());
            self.body.write_all(&buf).map_err(Flare::from)?;
        } else {
            unblock!(buf, || {
                debug!("to write buf (blocking, len: {})", buf.remaining());
                self.body.write_all(&buf)
            })
        }

        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Poll<(), Flare> {
        Ok(Async::Ready(()))
    }

    fn close(&mut self) -> Poll<(), Flare> {
        Ok(Async::Ready(()))
    }
}
