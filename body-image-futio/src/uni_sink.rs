use bytes::Buf;
use futures::{Async, AsyncSink, Poll, Sink, StartSend};
use tao_log::debug;
use tokio_threadpool;

use body_image::{BodyError, BodySink, Tunables};

#[cfg(feature = "futures03")] use {
    std::pin::Pin,
    std::task::{Context, Poll as Poll03},
    futures03::sink::Sink as Sink03,
};

use crate::{FutioError, UniBodyBuf};

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
        Err(e) => return Err(FutioError::Other(Box::new(e)))
    })
}

impl Sink for UniBodySink {
    type SinkItem = UniBodyBuf;
    type SinkError = FutioError;

    fn start_send(&mut self, buf: UniBodyBuf)
        -> StartSend<UniBodyBuf, FutioError>
    {
        let new_len = self.body.len() + (buf.remaining() as u64);
        if new_len > self.tune.max_body() {
            return Err(BodyError::BodyTooLong(new_len).into());
        }
        if self.body.is_ram() && new_len > self.tune.max_body_ram() {
            unblock!(buf, || {
                debug!("to write back file (blocking, len: {})", new_len);
                self.body.write_back(self.tune.temp_dir())
            })
        }
        if self.body.is_ram() {
            debug!("to save buf (len: {})", buf.remaining());
            self.body.write_all(&buf).map_err(FutioError::from)?;
        } else {
            unblock!(buf, || {
                debug!("to write buf (blocking, len: {})", buf.remaining());
                self.body.write_all(&buf)
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

#[cfg(feature = "futures03")]
macro_rules! unblock_03 {
    ($c:ident, || $b:block) => (match tokio_threadpool::blocking(|| $b) {
        Ok(Async::Ready(Ok(_))) => (),
        Ok(Async::Ready(Err(e))) => return Err(e.into()),
        Ok(Async::NotReady) => {
            // FIXME: Can't give the UniBodyBuf back, so no choice but panic!?
            // See also FIXME below.
            panic!("No blocking backup thread available");
        }
        Err(e) => return Err(FutioError::Other(Box::new(e)))
    })
}

#[cfg(feature = "futures03")]
impl Sink03<UniBodyBuf> for UniBodySink {
    type SinkError = FutioError;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll03<Result<(), FutioError>>
    {
        // FIXME: Can't really decide this, because we don't know the length of
        // the next item?!
        //
        // However, see current implementation (0.3.0-alpha.16) of
        // `futures_util::compat::Compat01As03Sink`.  It looks like the only,
        // rather complicated, way to deal with this fully is to only buffer
        // one item in `start_send`, then actually do the potentially blocking
        // parts in poll_ready, flush, close?
        Poll03::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, buf: UniBodyBuf)
        -> Result<(), FutioError>
    {
        let this = self.get_mut();

        let new_len = this.body.len() + (buf.remaining() as u64);
        if new_len > this.tune.max_body() {
            return Err(BodyError::BodyTooLong(new_len).into());
        }
        if this.body.is_ram() && new_len > this.tune.max_body_ram() {
            unblock_03!(buf, || {
                debug!("to write back file (blocking, len: {})", new_len);
                this.body.write_back(this.tune.temp_dir())
            })
        }
        if this.body.is_ram() {
            debug!("to save buf (len: {})", buf.remaining());
            this.body.write_all(&buf).map_err(FutioError::from)?;
        } else {
            unblock_03!(buf, || {
                debug!("to write buf (blocking, len: {})", buf.remaining());
                this.body.write_all(&buf)
            })
        }

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll03<Result<(), FutioError>>
    {
        Poll03::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll03<Result<(), FutioError>>
    {
        Poll03::Ready(Ok(()))
    }
}
