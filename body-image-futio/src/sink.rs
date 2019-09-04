use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use blocking_permit::{
    blocking_permit_future, BlockingPermitFuture,
    dispatch_rx, DispatchBlocking,
    IsReactorThread,
};
use futures::sink::Sink;
use hyper;
use tao_log::debug;

use body_image::{BodyError, BodySink, Tunables};

use crate::{BLOCKING_SET, FutioError};

/// Trait construction of `Sink` wrapper types.
pub trait SinkWrapper<T>: Sink<T> {
    /// Wrap by consuming a `BodySink` and `Tunables` instances.
    ///
    /// *Note*: `Tunables` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    fn new(body: BodySink, tune: Tunables) -> Self;

    /// Unwrap and return the `BodySink`.
    ///
    /// ## Panics
    ///
    /// May panic if called after a `Result::Err` is returned from any `Sink`
    /// method or before `Sink::poll_flush` or `Sink::poll_close` is called.
    fn into_inner(self) -> BodySink;
}

/// Adaptor for `BodySink` implementing the `futures::Sink` trait.  This
/// allows a `hyper::Body` (`hyper::Chunk` item) stream to be forwarded
/// (e.g. via `futures::Stream::forward`) to a `BodySink`, in a fully
/// asynchronous fashion.
///
/// `Tunables` are used during the streaming to decide when to write back a
/// BodySink in `Ram` to `FsWrite`. This implementation uses permits or a
/// dispatch pool for blocking operations including `BodySink::write_back` and
/// `BodySink::write_all` (state `FsWrite`).
#[derive(Debug)]
pub struct AsyncBodySink {
    body: Option<BodySink>,
    delegate: Delegate,
    tune: Tunables,
    buf: Option<hyper::Chunk>
}

#[derive(Debug)]
enum Delegate {
    Dispatch(DispatchBlocking<(Result<(), BodyError>, BodySink)>),
    Permit(BlockingPermitFuture<'static>),
    None
}

impl AsyncBodySink {
    /// Wrap by consuming a `BodySink` and `Tunables` instances.
    ///
    /// *Note*: `Tunables` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    pub fn new(body: BodySink, tune: Tunables) -> AsyncBodySink {
        AsyncBodySink {
            body: Some(body),
            delegate: Delegate::None,
            tune,
            buf: None
        }
    }

    /// Unwrap and return the `BodySink`.
    ///
    /// ## Panics
    ///
    /// May panic if called after a `Result::Err` is returned from any `Sink`
    /// method or before `Sink::poll_flush` or `Sink::poll_close` is called.
    pub fn into_inner(self) -> BodySink {
        self.body.expect("AsyncBodySink::into_inner called in incomplete state")
    }

    // This logically combines `Sink::poll_ready` and `Sink::start_send` into
    // one operation. If the item is returned, this is equivelent to
    // `Poll::Pending`, and the item will be later retried.
    fn poll_send(&mut self, cx: &mut Context<'_>, buf: hyper::Chunk)
        -> Result<Option<hyper::Chunk>, FutioError>
    {
        // Handle any delegate futures (new permit or early exit)
        let permit = match self.delegate {
            Delegate::Dispatch(ref mut db) => {
                match Pin::new(&mut *db).poll(cx) {
                    Poll::Pending => return Ok(Some(buf)),
                    Poll::Ready(Err(e)) => return Err(FutioError::Other(Box::new(e))),
                    Poll::Ready(Ok((Ok(()), body))) => {
                        self.delegate = Delegate::None;
                        self.body = Some(body);
                        return Ok(None);
                    }
                    Poll::Ready(Ok((Err(e), body))) => {
                        // Same delegate should repeat error forever
                        self.body = Some(body);
                        return Err(e.into());
                    }
                }
            }
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

        // Nothing to do for an empty buffer (as used for re-poll, below)
        if buf.is_empty() {
            return Ok(None)
        }

        // Early exit if too long
        let new_len = self.body.as_ref().unwrap().len() + (buf.len() as u64);
        if new_len > self.tune.max_body() {
            return Err(BodyError::BodyTooLong(new_len).into());
        }

        // Ram doesn't need blocking permit (early exit)
        if self.body.as_ref().unwrap().is_ram() && new_len <= self.tune.max_body_ram() {
            debug!("to save buf (len: {})", buf.len());
            self.body.as_mut().unwrap().write_all(&buf).map_err(FutioError::from)?;
            return Ok(None)
        };

        // Otherwise we'll need a permit or to dispatch (and exit early)
        if permit.is_none() {
            match blocking_permit_future(&BLOCKING_SET) {
                Ok(f) => {
                    self.delegate = Delegate::Permit(f);
                    // Ensure re-poll with same chunk
                    cx.waker().wake_by_ref();
                    return Ok(Some(buf));
                }
                Err(IsReactorThread) => {
                    let mut body = self.body.take().unwrap();
                    let temp_dir = self.tune.temp_dir().to_owned();
                    self.delegate = Delegate::Dispatch(dispatch_rx(move || {
                        if body.is_ram() {
                            debug!("to write back file (dispatch, len: {})", new_len);
                            if let Err(e) = body.write_back(temp_dir) {
                                return (Err(e), body);
                            }
                        }
                        debug!("to write buf (dispatch, len: {})", buf.len());
                        let res = body.write_all(&buf);
                        (res, body)
                    }));
                    // Ensure re-poll with a new empty chunk
                    cx.waker().wake_by_ref();
                    return Ok(Some(vec!{}.into()));
                }
            }
        }

        // If still Ram at this point, needs to be written back (blocking)
        if self.body.as_ref().unwrap().is_ram() {
            permit.unwrap().run_unwrap(|| {
                debug!("to write back file (blocking, len: {})", new_len);
                self.body.as_mut().unwrap().write_back(self.tune.temp_dir())?;
                debug!("to write buf (blocking, len: {})", buf.len());
                self.body.as_mut().unwrap().write_all(&buf)
            })?;
        } else {
            permit.unwrap().run_unwrap(|| {
                debug!("to write buf (blocking, len: {})", buf.len());
                self.body.as_mut().unwrap().write_all(&buf)
            })?;
        }
        Ok(None)
    }
}

impl SinkWrapper<hyper::Chunk> for AsyncBodySink {
    fn new(body: BodySink, tune: Tunables) -> AsyncBodySink {
        AsyncBodySink::new(body, tune)
    }

    fn into_inner(self) -> BodySink {
        AsyncBodySink::into_inner(self)
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
