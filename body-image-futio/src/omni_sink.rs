use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use blocking_permit::{
    blocking_permit_future, BlockingPermitFuture,
    dispatch_rx, Dispatched, is_dispatch_pool_registered
};
use bytes::Bytes;
use futures_sink::Sink;
use tao_log::debug;

use body_image::{BodyError, BodySink};

use crate::{
    Blocking, BlockingArbiter, LenientArbiter, StatefulArbiter,
    FutioError, FutioTunables, SinkWrapper, OmniBuf,
};

// FIXME: Should our B bounds be different than OmniBuf?
// (Don't need its extension method)

/// Adaptor for `BodySink` implementing the `futures::Sink` trait.  This
/// allows a `hyper::Body` (`Bytes` item) stream to be forwarded
/// (e.g. via `futures::Stream::forward`) to a `BodySink`, in a fully
/// asynchronous fashion. FIXME
///
/// `FutioTunables` are used during the streaming to decide when to write back
/// a BodySink in `Ram` to `FsWrite`. This implementation uses permits or a
/// dispatch pool for blocking operations including `BodySink::write_back` and
/// `BodySink::write_all` (state `FsWrite`).
#[derive(Debug)]
pub struct OmniBodySink<B, BA=LenientArbiter>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>,
          BA: BlockingArbiter + Default + Unpin
{
    body: BodySink,
    tune: FutioTunables,
    arbiter: BA,
    buf: Option<B>
}

impl<B, BA> OmniBodySink<B, BA>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>,
          BA: BlockingArbiter + Default + Unpin
{
    /// Wrap by consuming a `BodySink` and `FutioTunables` instances.
    ///
    /// *Note*: `FutioTunables` is `Clone` (inexpensive), so that can be done
    /// beforehand to preserve an owned copy.
    pub fn new(body: BodySink, tune: FutioTunables) -> Self {
        OmniBodySink {
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
    fn poll_send(&mut self, _cx: &mut Context<'_>, buf: B)
        -> Result<Option<B>, FutioError>
    {
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
            debug!("to save buf (len: {})", buf.remaining());
            self.body.save(buf).map_err(FutioError::from)?;
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
}

impl<B, BA> SinkWrapper<B> for OmniBodySink<B, BA>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>,
          BA: BlockingArbiter + Default + Unpin
{
    fn new(body: BodySink, tune: FutioTunables) -> Self {
        OmniBodySink::new(body, tune)
    }

    fn into_inner(self) -> BodySink {
        OmniBodySink::into_inner(self)
    }
}

impl<B, BA> Sink<B> for OmniBodySink<B, BA>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>,
          BA: BlockingArbiter + Default + Unpin
{
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }

    fn start_send(self: Pin<&mut Self>, buf: B)
        -> Result<(), FutioError>
    {
        let this = unsafe { self.get_unchecked_mut() };
        assert!(this.buf.is_none());
        this.buf = Some(buf);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        let this = unsafe { self.get_unchecked_mut() };
        if let Some(buf) = this.buf.take() {
            match this.poll_send(cx, buf) {
                Ok(None) => Poll::Ready(Ok(())),
                Ok(s @ Some(_)) => {
                    this.buf = s;
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

pub struct PermitBodySink<B>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>,
{
    sink: OmniBodySink<B, StatefulArbiter>,
    permit: Option<BlockingPermitFuture<'static>>
}

macro_rules! with_permit {
    ($this:expr, $cx:expr, $doit:ident) => {{
        let this = $this;
        let permit = if let Some(ref mut pf) = this.permit {
            let pf = Pin::new(pf);
            match pf.poll($cx) {
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
            let sink = Pin::new(&mut this.sink);
            let res = p.run(|| sink.$doit($cx));
            debug_assert_eq!(this.sink.arbiter.state(), Blocking::Void);
            res
        } else {
            let res = {
                let sink = Pin::new(&mut this.sink);
                sink.$doit($cx)
            };

            if res.is_pending()
                && this.sink.arbiter.state() == Blocking::Pending
            {
                this.permit = Some(blocking_permit_future(
                    this.sink.tune.blocking_semaphore()
                        .expect("blocking semaphore required!")
                ));
            }
            res
        }
    }}
}

impl<B> Sink<B> for PermitBodySink<B>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>
{
    type Error = FutioError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        with_permit!(self.get_mut(), cx, poll_ready)
    }

    fn start_send(mut self: Pin<&mut Self>, buf: B)
        -> Result<(), FutioError>
    {
        Pin::new(&mut self.sink).start_send(buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        with_permit!(self.get_mut(), cx, poll_flush)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Result<(), FutioError>>
    {
        self.poll_flush(cx)
    }
}

impl<B> SinkWrapper<B> for PermitBodySink<B>
    where B: OmniBuf + Into<Bytes> + AsRef<[u8]>
{
    fn new(body: BodySink, tune: FutioTunables) -> Self {
        PermitBodySink {
            sink: OmniBodySink::new(body, tune),
            permit: None
        }
    }

    fn into_inner(self) -> BodySink {
        self.sink.into_inner()
    }
}
