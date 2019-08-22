use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use futures::sink::Sink;
use tao_log::{debug, warn};
use tokio_executor::threadpool as tokio_threadpool;

use body_image::{BodyError, BodySink, Tunables};

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
/// `Ok(AsyncSink::NotReady(chunk))`, until a backup thread becomes available
/// or any timeout occurs.
pub struct UniBodySink {
    body: BodySink,
    tune: Tunables,
    buf: Option<UniBodyBuf>
}

impl fmt::Debug for UniBodySink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UniBodySink")
            .field("body", &self.body)
            .field("tune", &self.tune)
            //FIXME: buf?
            .finish()
    }
}

impl UniBodySink {
    /// Wrap by consuming a `BodySink` and `Tunables` instances.
    ///
    /// *Note*: Both `BodyImage` and `Tunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    pub fn new(body: BodySink, tune: Tunables) -> UniBodySink {
        UniBodySink {
            body,
            tune,
            buf: None
        }
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
        Poll::Pending => {
            warn!("UniBodySink: no blocking backup thread available");
            return Ok(Some($c));
        }
        Poll::Ready(Ok(Ok(_))) => (),
        Poll::Ready(Ok(Err(e))) => return Err(e.into()),
        Poll::Ready(Err(e)) => return Err(FutioError::Other(Box::new(e)))
    })
}

impl UniBodySink {
    // This logically combines `Sink::poll_ready` and `Sink::start_send` into
    // one operation. If the item is returned, this is equivelent to
    // `Poll::Pending`, and the item will be later retried.
    fn poll_send(&mut self, _cx: &mut Context<'_>, buf: UniBodyBuf)
        -> Result<Option<UniBodyBuf>, FutioError>
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
            self.body.write_all(&buf).map_err(FutioError::from)?
        } else {
            unblock!(buf, || {
                debug!("to write buf (blocking, len: {})", buf.remaining());
                self.body.write_all(&buf)
            })
        }

        Ok(None)
    }
}

impl Sink<UniBodyBuf> for UniBodySink {
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, buf: UniBodyBuf)
        -> Result<(), FutioError>
    {
        assert!(self.buf.is_none());
        self.buf = Some(buf);
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        if let Some(buf) = self.buf.take() {
            match self.poll_send(cx, buf) {
                Ok(None) => Poll::Ready(Ok(())),
                Ok(s @ Some(_)) => {
                    self.buf = s;

                    // Might be needed, until Tokio offers to wake for us, as part
                    // of `tokio_threadpool::blocking`
                    cx.waker().wake_by_ref();

                    Poll::Pending
                }
                Err(e) => Poll::Ready(Err(e))
            }
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }
}
