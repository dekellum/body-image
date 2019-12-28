use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use blocking_permit::{
    blocking_permit_future, BlockingPermitFuture,
    dispatch_rx, Dispatched,
};
use bytes::{Buf, Bytes};
use futures_sink::Sink;
use tao_log::debug;

use body_image::{BodyError, BodySink};

use crate::{
    Blocking, BlockingArbiter, LenientArbiter, StatefulArbiter,
    FutioError, FutioTunables, SinkWrapper, UniBodyBuf,
};

/// Marker trait for satisfying Sink input buffer requirements.
pub trait InputBuf: Buf + AsRef<[u8]> + Into<Bytes>
    + 'static + Send + Sync + Unpin {}

impl InputBuf for Bytes {}
impl InputBuf for UniBodyBuf {}

/// Adaptor for `BodySink` implementing the `futures::Sink` trait.  This allows
/// a `hyper::Body` (`Bytes` item) or `AsyncBodyStream` to be forwarded
/// (e.g. via `futures::Stream::forward`) to a `BodySink`, in a fully
/// asynchronous fashion.
///
/// `FutioTunables` are used during the streaming to decide when to write back
/// a BodySink in `Ram` to `FsWrite`.
#[derive(Debug)]
pub struct AsyncBodySink<B, BA=LenientArbiter>
    where B: InputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    body: BodySink,
    tune: FutioTunables,
    arbiter: BA,
    buf: Option<B>
}

impl<B, BA> AsyncBodySink<B, BA>
    where B: InputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    /// Wrap by consuming a `BodySink` and `FutioTunables` instances.
    ///
    /// *Note*: `FutioTunables` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    pub fn new(body: BodySink, tune: FutioTunables) -> Self {
        AsyncBodySink {
            body,
            tune,
            arbiter: BA::default(),
            buf: None
        }
    }

    /// Unwrap and return the `BodySink`.
    pub fn into_inner(self) -> BodySink {
        self.body
    }

    // This logically combines `Sink::poll_ready` and `Sink::start_send` into
    // one operation. If the item is returned, this is equivalent to
    // `Poll::Pending`, and the item will be later retried.
    fn poll_send(&mut self, buf: B) -> Result<Option<B>, FutioError> {
        if buf.remaining() == 0 {
            return Ok(None)
        }

        // Early exit if too long
        let new_len = self.body.len() + (buf.remaining() as u64);
        if new_len > self.tune.image().max_body() {
            return Err(BodyError::BodyTooLong(new_len).into());
        }

        // Ram doesn't need blocking permit (early exit)
        if self.body.is_ram() && new_len <= self.tune.image().max_body_ram()
        {
            debug!("to push buf (len: {})", buf.remaining());
            self.body.push(buf).map_err(FutioError::from)?;
            return Ok(None)
        };

        // Otherwise blocking is required, check if its allowed. If not we
        // return the buf, expecting arbitration above us.
        if !self.arbiter.can_block() {
            return Ok(Some(buf));
        }

        // Blocking allowed
        // If still Ram at this point, needs to be written back
        if self.body.is_ram() {
            debug!("to write back file (blocking, len: {})", new_len);
            self.body.write_back(self.tune.image().temp_dir())?;
        }

        // Now write the buf
        debug!("to write buf (blocking, len: {})", buf.remaining());
        self.body.write_all(&buf)?;

        Ok(None)
    }

    fn poll_flush_impl(&mut self) -> Poll<Result<(), FutioError>> {
        if let Some(buf) = self.buf.take() {
            match self.poll_send(buf) {
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

    fn send(&mut self, buf: B) -> Result<(), FutioError> {
        assert!(self.buf.is_none());
        self.buf = Some(buf);
        Ok(())
    }

}

impl<B, BA> SinkWrapper<B> for AsyncBodySink<B, BA>
    where B: InputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    fn new(body: BodySink, tune: FutioTunables) -> Self {
        AsyncBodySink::new(body, tune)
    }

    fn into_inner(self) -> BodySink {
        AsyncBodySink::into_inner(self)
    }
}

impl<B, BA> Sink<B> for AsyncBodySink<B, BA>
    where B: InputBuf,
          BA: BlockingArbiter + Default + Unpin
{
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.get_mut().poll_flush_impl()
    }

    fn start_send(self: Pin<&mut Self>, buf: B)
        -> Result<(), FutioError>
    {
        self.get_mut().send(buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.get_mut().poll_flush_impl()
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.get_mut().poll_flush_impl()
    }
}

pub struct PermitBodySink<B>
    where B: InputBuf,
{
    sink: AsyncBodySink<B, StatefulArbiter>,
    permit: Option<BlockingPermitFuture<'static>>
}

impl<B> SinkWrapper<B> for PermitBodySink<B>
    where B: InputBuf
{
    fn new(body: BodySink, tune: FutioTunables) -> Self {
        PermitBodySink {
            sink: AsyncBodySink::new(body, tune),
            permit: None
        }
    }

    fn into_inner(self) -> BodySink {
        self.sink.into_inner()
    }
}

impl<B> Sink<B> for PermitBodySink<B>
    where B: InputBuf
{
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, buf: B) -> Result<(), FutioError> {
        self.sink.send(buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        let this = self.get_mut();
        let permit = if let Some(ref mut pf) = this.permit {
            let pf = Pin::new(pf);
            match pf.poll(cx) {
                Poll::Ready(Ok(p)) => {
                    this.permit = None;
                    Some(p)
                }
                Poll::Ready(Err(e)) => {
                    // FIXME: Improve this error?
                    return Poll::Ready(Err(FutioError::Other(Box::new(e))));
                }
                Poll::Pending => {
                    return Poll::Pending
                }
            }
        } else {
            None
        };

        if let Some(p) = permit {
            this.sink.arbiter.set(Blocking::Once);
            let sink = &mut this.sink;
            let res = p.run(|| sink.poll_flush_impl());
            debug_assert_eq!(this.sink.arbiter.state(), Blocking::Void);
            res
        } else {
            let res = this.sink.poll_flush_impl();
            if res.is_pending()
                && this.sink.arbiter.state() == Blocking::Pending
            {
                this.permit = Some(blocking_permit_future(
                    this.sink.tune.blocking_semaphore()
                        .expect("blocking semaphore required!")
                ));

                // Recurse for correct waking
                return Pin::new(this).poll_flush(cx);
            }
            res
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }
}

pub struct DispatchBodySink<B>
    where B: InputBuf
{
    state: DispatchState<B>,
}

enum DispatchState<B>
    where B: InputBuf
{
    Sink(Option<AsyncBodySink<B, StatefulArbiter>>),
    Dispatch(Dispatched<(
        Poll<Result<(), FutioError>>,
        AsyncBodySink<B, StatefulArbiter>)>),
}

impl<B> SinkWrapper<B> for DispatchBodySink<B>
    where B: InputBuf
{
    fn new(body: BodySink, tune: FutioTunables) -> Self {
        DispatchBodySink {
            state: DispatchState::Sink(Some(AsyncBodySink::new(body, tune))),
        }
    }

    fn into_inner(self) -> BodySink {
        match self.state {
            DispatchState::Sink(sobs) => {
                sobs.unwrap().into_inner()
            }
            DispatchState::Dispatch(_) => {
                panic!("Can't recover inner BodySink from dispatched!");
            }
        }
    }
}

impl<B> Sink<B> for DispatchBodySink<B>
    where B: InputBuf
{
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }

    fn start_send(self: Pin<&mut Self>, buf: B) -> Result<(), FutioError> {
        let this = self.get_mut();
        match this.state {
            DispatchState::Sink(ref mut obo) => {
                obo.as_mut().unwrap().send(buf)
            }
            DispatchState::Dispatch(_) => {
                // shouldn't happen
                panic!("DispatchBodySink::start_send called while dispatched!")
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        let this = self.get_mut();
        match this.state {
            DispatchState::Sink(ref mut obo) => {
                let ob = obo.as_mut().unwrap();
                let res = ob.poll_flush_impl();
                if res.is_pending() && ob.arbiter.state() == Blocking::Pending {
                    ob.arbiter.set(Blocking::Once);
                    let mut ob = obo.take().unwrap();
                    this.state = DispatchState::Dispatch(dispatch_rx(move || {
                        (ob.poll_flush_impl(), ob)
                    }).unwrap());

                    // Recurse for correct waking
                    return Pin::new(this).poll_flush(cx);
                }
                res
            }
            DispatchState::Dispatch(ref mut db) => {
                let (res, ob) = match Pin::new(&mut *db).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => {
                        // FIXME: Improve this error?
                        return Poll::Ready(Err(FutioError::Other(Box::new(e))));
                    }
                    Poll::Ready(Ok((res, ob))) => (res, ob),
                };
                debug_assert_eq!(ob.arbiter.state(), Blocking::Void);
                this.state = DispatchState::Sink(Some(ob));
                res
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }
}
