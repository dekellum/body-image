use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use blocking_permit::{
    blocking_permit_future,
    BlockingPermitFuture,
    IsReactorThread,
};
use futures::sink::Sink;
use hyper;
use tao_log::debug;

use body_image::{BodyError, BodySink, Tunables};

use crate::{BLOCKING_SET, FutioError};

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
    delegate: Delegate,
    tune: Tunables,
    buf: Option<hyper::Chunk>
}

#[derive(Debug)]
enum Delegate {
    Permit(BlockingPermitFuture<'static>),
    None
}

impl AsyncBodySink {
    /// Wrap by consuming a `BodySink` and `Tunables` instances.
    ///
    /// *Note*: Both `BodyImage` and `Tunables` are `Clone` (inexpensive), so
    /// that can be done beforehand to preserve owned copies.
    pub fn new(body: BodySink, tune: Tunables) -> AsyncBodySink {
        AsyncBodySink {
            body,
            delegate: Delegate::None,
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

impl AsyncBodySink {
    // This logically combines `Sink::poll_ready` and `Sink::start_send` into
    // one operation. If the item is returned, this is equivelent to
    // `Poll::Pending`, and the item will be later retried.
    fn poll_send(&mut self, cx: &mut Context<'_>, buf: hyper::Chunk)
        -> Result<Option<hyper::Chunk>, FutioError>
    {
        // Handle any delegate futures (new permit or early exit)
        let permit = match self.delegate {
            Delegate::Permit(ref mut pf) => {
                match Pin::new(pf).poll(cx) {
                    Poll::Pending => return Ok(Some(buf)),
                    Poll::Ready(Ok(p)) => {
                        self.delegate = Delegate::None;
                        Some(p)
                    }
                    Poll::Ready(Err(e)) => {
                        // TODO: Better error?
                        return Err(FutioError::Other(Box::new(e)));
                    }
                }
            }
            Delegate::None => None
        };

        // Early exit if too long
        let new_len = self.body.len() + (buf.len() as u64);
        if new_len > self.tune.max_body() {
            return Err(BodyError::BodyTooLong(new_len).into());
        }

        // Ram doesn't need blocking permit (early exit)
        if self.body.is_ram() && new_len <= self.tune.max_body_ram() {
            debug!("to save buf (len: {})", buf.len());
            self.body.write_all(&buf).map_err(FutioError::from)?;
            return Ok(None)
        };

        // Otherwise we'll need a permit or to dispatch (and exit early)
        if permit.is_none() {
            match blocking_permit_future(&BLOCKING_SET) {
                Ok(f) => {
                    self.delegate = Delegate::Permit(f);
                }
                Err(IsReactorThread) => unimplemented!(),
            }
            // recurse once (needed for correct waking)
            return Pin::new(self).poll_send(cx, buf);
        }

        // If still Ram at this point, needs to be written back (blocking)
        if self.body.is_ram() {
            permit.unwrap().run_unwrap(|| {
                debug!("to write back file (blocking, len: {})", new_len);
                self.body.write_back(self.tune.temp_dir())?;
                debug!("to write buf (blocking, len: {})", buf.len());
                self.body.write_all(&buf)
            })?;
        } else {
            permit.unwrap().run_unwrap(|| {
                debug!("to write buf (blocking, len: {})", buf.len());
                self.body.write_all(&buf)
            })?;
        }
        Ok(None)
    }
}

impl Sink<hyper::Chunk> for AsyncBodySink {
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, buf: hyper::Chunk)
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
