// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ZFS dataset-transfer streamers (LM-4).
//!
//! The agent-controlled alternative to `zfs send | ssh peer "zfs
//! recv"`: source tritonagent pipes `zfs send` stdout into
//! [`ZfsSender`] which writes [`Message::ZfsChunk`] frames over a
//! [`Transport`] (its own dedicated WebSocket — the
//! `GET /migrate/{id}/zfs` route on the target listener), and the
//! target tritonagent pipes [`ZfsReceiver`] into `zfs recv` stdin.
//!
//! ```text
//!  source agent                                      target agent
//!  ────────────                                      ────────────
//!  zfs send -i base @next                            zfs recv pool/zone
//!    │ stdout                                              ▲ stdin
//!    ▼                                                     │
//!  ZfsSender ──► WebSocket (binary frames) ──► ZfsReceiver ┘
//!                ZfsChunk(…) … ZfsChunk(…) ZfsEnd
//! ```
//!
//! Both streamers are generic over [`Transport`] so the loopback
//! tests connect them via the in-memory channel pair without any
//! WebSocket or subprocess involvement.
//!
//! ## Cancellation + cleanup
//!
//! * **Source side**: if the underlying transport drops, the
//!   `AsyncWrite::poll_write` returns an error and `ZfsSender::run`
//!   surfaces it. The caller is expected to kill / await the
//!   `zfs send` subprocess so its stdout pipe closes.
//! * **Target side**: a clean `ZfsEnd` lets the receiver drop
//!   `zfs recv`'s stdin so the child exits 0. A network drop (no
//!   `ZfsEnd`) surfaces as `MigrateError::PeerClosed`; the caller
//!   should kill `zfs recv` and roll back the partial receive.
//!
//! The receiver does NOT consume `Message::ZfsChunk` with zero
//! payload as a sentinel — only `ZfsEnd` signals EOS — because
//! `zfs send` is allowed to emit zero-byte reads from its stdout
//! at chunk boundaries and we don't want to confuse "small chunk"
//! with "transfer complete".

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::warn;

use crate::codec::Message;
use crate::protocol::ZFS_CHUNK_SIZE;
use crate::state_machine::MigrateError;
use crate::transport::Transport;

/// Per-chunk progress observer. Invoked with the cumulative byte
/// count after each chunk crosses the transport; the agent's
/// throttled progress reporter hangs off this.
pub type ProgressFn = Box<dyn FnMut(u64) + Send>;

/// Source-side streamer. Reads the `zfs send` stdout pipe in
/// [`ZFS_CHUNK_SIZE`] windows and writes each as a
/// [`Message::ZfsChunk`] over the transport. Sends [`Message::ZfsEnd`]
/// on clean EOS and closes the transport.
pub struct ZfsSender<T, R> {
    transport: T,
    reader: R,
    progress: Option<ProgressFn>,
}

impl<T, R> ZfsSender<T, R>
where
    T: Transport,
    R: AsyncRead + Unpin + Send,
{
    pub fn new(transport: T, reader: R) -> Self {
        Self {
            transport,
            reader,
            progress: None,
        }
    }

    /// Observe streaming progress: `callback` fires with the
    /// cumulative byte count after each chunk is sent. Callbacks
    /// run inline on the stream loop, so keep them cheap (store an
    /// atomic, poke a channel); blocking here stalls the transfer.
    #[must_use]
    pub fn with_progress(mut self, callback: impl FnMut(u64) + Send + 'static) -> Self {
        self.progress = Some(Box::new(callback));
        self
    }

    /// Drive the source half to completion. Returns the total
    /// number of bytes streamed (useful for the migration's
    /// progress callbacks).
    ///
    /// The send loop reads from `reader` in fixed-size windows; a
    /// short read returns `Ok(N)` where `N < ZFS_CHUNK_SIZE` and
    /// we still emit one chunk for it — `zfs send` produces
    /// records of varying sizes and we don't try to coalesce them
    /// into uniform chunks (saves a memmove per record). EOF
    /// (`Ok(0)`) is the trigger to emit [`Message::ZfsEnd`].
    pub async fn run(mut self) -> Result<u64, MigrateError> {
        let mut buf = vec![0u8; ZFS_CHUNK_SIZE];
        let mut total: u64 = 0;
        loop {
            let n = self
                .reader
                .read(&mut buf)
                .await
                .map_err(MigrateError::Transport)?;
            if n == 0 {
                break;
            }
            // Allocate a fresh Vec from the slice so the
            // outbound message owns its bytes; the read buffer is
            // reusable next iteration. (We pay one copy per
            // chunk — see crate-doc comment for the perf
            // discussion; LM-10 can swap to `bytes::Bytes` if
            // benchmarks demand it.)
            let chunk = buf[..n].to_vec();
            self.transport.send(Message::ZfsChunk(chunk)).await?;
            total += n as u64;
            if let Some(cb) = self.progress.as_mut() {
                cb(total);
            }
        }
        self.transport.send(Message::ZfsEnd).await?;
        let _ = self.transport.close().await;
        Ok(total)
    }
}

/// Target-side streamer. Reads [`Message::ZfsChunk`] frames off
/// the transport and writes each into `writer` (`zfs recv` stdin).
/// Returns the total number of bytes received on clean EOS.
pub struct ZfsReceiver<T, W> {
    transport: T,
    writer: W,
    progress: Option<ProgressFn>,
}

impl<T, W> ZfsReceiver<T, W>
where
    T: Transport,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(transport: T, writer: W) -> Self {
        Self {
            transport,
            writer,
            progress: None,
        }
    }

    /// Observe receive progress: `callback` fires with the
    /// cumulative byte count after each chunk lands in `writer`.
    /// Same inline-and-cheap contract as
    /// [`ZfsSender::with_progress`].
    #[must_use]
    pub fn with_progress(mut self, callback: impl FnMut(u64) + Send + 'static) -> Self {
        self.progress = Some(Box::new(callback));
        self
    }

    /// Drive the target half to completion. Returns the total
    /// number of bytes consumed.
    ///
    /// Surfaces specific [`MigrateError`] kinds:
    ///
    /// * [`MigrateError::PeerClosed`] — transport ended without
    ///   `ZfsEnd`; the caller should abort the migration + kill
    ///   `zfs recv` so the partial dataset is rolled back.
    /// * [`MigrateError::Unexpected`] — non-`ZfsChunk`/`ZfsEnd`
    ///   message landed (programming error: someone wired the
    ///   memory-channel transport to this side).
    pub async fn run(mut self) -> Result<u64, MigrateError> {
        let mut total: u64 = 0;
        loop {
            match self.transport.recv().await? {
                Some(Message::ZfsChunk(data)) => {
                    if data.is_empty() {
                        // `zfs send` can legitimately produce
                        // zero-byte writes at record boundaries.
                        // Skip the writer call (`AsyncWrite::write_all`
                        // would still no-op but the skip avoids a
                        // syscall on hot streams).
                        continue;
                    }
                    self.writer
                        .write_all(&data)
                        .await
                        .map_err(MigrateError::Transport)?;
                    total += data.len() as u64;
                    if let Some(cb) = self.progress.as_mut() {
                        cb(total);
                    }
                }
                Some(Message::ZfsEnd) => {
                    if let Err(e) = self.writer.flush().await {
                        warn!(error = %e, "zfs_stream: flush after ZfsEnd failed");
                    }
                    // Shutdown signals "no more bytes" to the
                    // child's stdin so `zfs recv` exits.
                    let _ = self.writer.shutdown().await;
                    let _ = self.transport.close().await;
                    return Ok(total);
                }
                Some(other) => {
                    return Err(MigrateError::Unexpected {
                        phase: "zfs-stream",
                        got: variant_name(&other),
                    });
                }
                None => {
                    return Err(MigrateError::PeerClosed {
                        phase: "zfs-stream",
                    });
                }
            }
        }
    }
}

fn variant_name(msg: &Message) -> &'static str {
    match msg {
        Message::Okay => "Okay",
        Message::Error(_) => "Error",
        Message::Serialized(_) => "Serialized",
        Message::PageBatch { .. } => "PageBatch",
        Message::MemFetch(_) => "MemFetch",
        Message::MemEnd => "MemEnd",
        Message::MemDone => "MemDone",
        Message::PauseSignal => "PauseSignal",
        Message::RamHash(_) => "RamHash",
        Message::PauseComplete(_) => "PauseComplete",
        Message::SwitchComplete(_) => "SwitchComplete",
        Message::ZfsChunk(_) => "ZfsChunk",
        Message::ZfsEnd => "ZfsEnd",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::inmem;
    use std::io;
    use std::io::Cursor;

    /// A `tokio::io::AsyncWrite` collector backed by a `Vec<u8>`.
    /// Tests pipe `ZfsReceiver` into it and assert the captured
    /// bytes match the source pattern.
    struct VecWriter(Vec<u8>);

    impl AsyncWrite for VecWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            self.0.extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn loopback_round_trips_known_pattern() {
        // Source: 1 MiB pattern (forces multi-chunk).
        let pattern: Vec<u8> = (0..1024 * 1024).map(|i| (i & 0xff) as u8).collect();
        let (src_t, dst_t) = inmem::channel_pair(16);
        let writer = VecWriter(Vec::new());

        let sender_pattern = pattern.clone();
        let send_task = tokio::spawn(async move {
            let cursor = Cursor::new(sender_pattern);
            let sender = ZfsSender::new(src_t, cursor);
            sender.run().await
        });

        let recv_task = tokio::spawn(async move {
            let receiver = ZfsReceiver::new(dst_t, writer);
            receiver.run().await
        });

        // We need to recover the writer to inspect its buffer.
        // VecWriter is owned by ZfsReceiver, so use a channel.
        // Instead, restructure: have the receiver consume the
        // writer and return total; pattern compare happens
        // inside the recv_task via a wrapper. For simplicity
        // here, just compare sent/received totals.
        let sent = send_task.await.expect("sender join").expect("sender run");
        let recvd = recv_task.await.expect("recv join").expect("recv run");
        assert_eq!(sent, pattern.len() as u64);
        assert_eq!(recvd, pattern.len() as u64);
    }

    /// Variant that asserts the bytes themselves match end-to-end.
    /// Uses an `Arc<Mutex<Vec<u8>>>`-backed writer so the test
    /// task can read what was received after the task ends.
    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl AsyncWrite for SharedWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn loopback_bytes_match_end_to_end() {
        let pattern: Vec<u8> = (0..3 * ZFS_CHUNK_SIZE + 17)
            .map(|i| (i & 0xff) as u8)
            .collect();
        let (src_t, dst_t) = inmem::channel_pair(32);
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let sender_pattern = pattern.clone();
        let send = tokio::spawn(async move {
            let sender = ZfsSender::new(src_t, Cursor::new(sender_pattern));
            sender.run().await
        });
        let received_clone = received.clone();
        let recv = tokio::spawn(async move {
            let receiver = ZfsReceiver::new(dst_t, SharedWriter(received_clone));
            receiver.run().await
        });
        send.await.expect("send join").expect("send run");
        recv.await.expect("recv join").expect("recv run");

        let got = received.lock().unwrap().clone();
        assert_eq!(got.len(), pattern.len(), "byte-count mismatch");
        assert_eq!(got, pattern, "byte-pattern mismatch");
    }

    #[tokio::test]
    async fn progress_callbacks_observe_cumulative_totals() {
        let pattern: Vec<u8> = (0..2 * ZFS_CHUNK_SIZE + 5)
            .map(|i| (i & 0xff) as u8)
            .collect();
        let (src_t, dst_t) = inmem::channel_pair(16);

        let sent_samples = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let recv_samples = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));

        let sender_pattern = pattern.clone();
        let sent_clone = sent_samples.clone();
        let send = tokio::spawn(async move {
            ZfsSender::new(src_t, Cursor::new(sender_pattern))
                .with_progress(move |total| sent_clone.lock().unwrap().push(total))
                .run()
                .await
        });
        let recv_clone = recv_samples.clone();
        let recv = tokio::spawn(async move {
            ZfsReceiver::new(dst_t, VecWriter(Vec::new()))
                .with_progress(move |total| recv_clone.lock().unwrap().push(total))
                .run()
                .await
        });
        let sent = send.await.expect("send join").expect("send run");
        let recvd = recv.await.expect("recv join").expect("recv run");
        assert_eq!(sent, pattern.len() as u64);
        assert_eq!(recvd, pattern.len() as u64);

        // Cumulative, strictly increasing, and the last sample is
        // the byte total on both ends.
        for samples in [&sent_samples, &recv_samples] {
            let s = samples.lock().unwrap();
            assert!(!s.is_empty());
            assert!(s.windows(2).all(|w| w[0] < w[1]), "not increasing: {s:?}");
            assert_eq!(*s.last().unwrap(), pattern.len() as u64);
        }
    }

    #[tokio::test]
    async fn empty_stream_round_trips() {
        // `zfs send` of an empty dataset is plausible during test
        // fixtures; the stream must still negotiate cleanly.
        let (src_t, dst_t) = inmem::channel_pair(4);
        let send = tokio::spawn(async move {
            let sender = ZfsSender::new(src_t, Cursor::new(Vec::<u8>::new()));
            sender.run().await
        });
        let recv = tokio::spawn(async move {
            let receiver = ZfsReceiver::new(dst_t, VecWriter(Vec::new()));
            receiver.run().await
        });
        assert_eq!(send.await.expect("send").expect("send run"), 0);
        assert_eq!(recv.await.expect("recv").expect("recv run"), 0);
    }

    #[tokio::test]
    async fn receiver_surfaces_peer_close_without_zfs_end() {
        // Source transport dropped before sending ZfsEnd — the
        // receiver must surface PeerClosed so the caller knows
        // to roll back the partial dataset.
        let (src_t, dst_t) = inmem::channel_pair(4);
        drop(src_t); // peer goes away without saying goodbye

        let writer = VecWriter(Vec::new());
        let result = ZfsReceiver::new(dst_t, writer).run().await;
        assert!(matches!(
            result,
            Err(MigrateError::PeerClosed {
                phase: "zfs-stream"
            })
        ));
    }
}
