// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared framing and I/O helpers for the Kelp relay tunnel.
//!
//! All three relay components (server, agent, bridge) share:
//! - [`WsCompat`] — adapts a WebSocket stream to `futures::io` byte I/O for yamux
//! - [`read_connect_target`] / [`write_connect_target`] — stream target framing
//! - [`bridge`] — bidirectional byte pump between two tokio I/O streams

use bytes::Bytes;
use futures_util::{Sink, Stream};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// Adapts a `WebSocketStream` (futures `Stream + Sink`) to the
/// `futures::io::AsyncRead + AsyncWrite` interface that yamux requires.
///
/// Binary frames carry data; all other frame types (text, ping, pong, close)
/// are silently skipped on reads. Each `poll_write` call sends one binary frame.
pub struct WsCompat<S> {
    inner: S,
    read_buf: Bytes,
}

impl<S> WsCompat<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            read_buf: Bytes::new(),
        }
    }
}

impl<S> futures_util::io::AsyncRead for WsCompat<S>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            if !this.read_buf.is_empty() {
                let n = buf.len().min(this.read_buf.len());
                buf[..n].copy_from_slice(&this.read_buf[..n]);
                this.read_buf = this.read_buf.slice(n..);
                return Poll::Ready(Ok(n));
            }
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                    this.read_buf = data;
                }
                // Skip text/ping/pong/close frames — yamux only uses binary.
                Poll::Ready(Some(Ok(_))) => continue,
                Poll::Ready(None) => return Poll::Ready(Ok(0)),
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S> futures_util::io::AsyncWrite for WsCompat<S>
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e)));
            }
            Poll::Pending => return Poll::Pending,
        }
        match Pin::new(&mut this.inner).start_send(Message::binary(buf.to_vec())) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, e))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .poll_flush(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .poll_close(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

/// Read the `host:port\n` target line from the start of a yamux stream.
pub async fn read_connect_target<R>(reader: &mut R) -> anyhow::Result<String>
where
    R: futures_util::io::AsyncRead + Unpin,
{
    use futures_util::io::AsyncReadExt as _;
    let mut buf = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await?;
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    String::from_utf8(buf).map_err(|e| anyhow::anyhow!("target contains invalid UTF-8: {e}"))
}

/// Write the `host:port\n` target line to the start of a yamux stream.
pub async fn write_connect_target<W>(writer: &mut W, target: &str) -> anyhow::Result<()>
where
    W: futures_util::io::AsyncWrite + Unpin,
{
    use futures_util::io::AsyncWriteExt as _;
    let line = format!("{target}\n");
    writer.write_all(line.as_bytes()).await?;
    Ok(())
}

/// Bidirectional byte pump between two tokio async I/O streams.
///
/// Callers bridging `yamux::Stream` (which implements `futures::io`) to a
/// `tokio::net::TcpStream` should first wrap the yamux stream with
/// `tokio_util::compat::FuturesAsyncReadCompatExt::compat()`.
pub async fn bridge<A, B>(a: &mut A, b: &mut B) -> anyhow::Result<(u64, u64)>
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(a, b)
        .await
        .map_err(Into::into)
}
