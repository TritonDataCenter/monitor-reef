// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Transport abstraction for the migration state machines.
//!
//! The state machines (`OutboundMigration`, `InboundMigration`) bind
//! to this trait rather than to `tokio_tungstenite::WebSocketStream`
//! directly so that:
//!
//! * The tritonagent migrate module plumbs a real WebSocket through
//!   when running on a CN (LM-3 follow-up).
//! * Unit + loopback tests connect two state machines via a pair of
//!   tokio mpsc channels (no WebSocket library involvement).
//! * Future transports (gRPC, raw QUIC, in-process) drop in without
//!   touching the state machines.
//!
//! Keep the surface tiny: send one [`Message`], receive one
//! [`Message`], close cleanly. Anything richer (backpressure
//! tuning, half-close) belongs on the transport itself, not in the
//! trait.

use async_trait::async_trait;
use std::io;

use crate::codec::Message;

/// What a state machine talks to. Implementations are paired (one
/// per side); they must agree on message ordering and framing so
/// what one side sends is what the other side receives.
#[async_trait]
pub trait Transport: Send {
    /// Send one [`Message`]. Returns when the message has been
    /// handed to the underlying transport (not necessarily when the
    /// peer has acked it). Cancellation safety: must be safe to drop
    /// the future at any await point.
    async fn send(&mut self, msg: Message) -> io::Result<()>;

    /// Receive one [`Message`]. `Ok(None)` means the peer cleanly
    /// closed; `Ok(Some(msg))` is a delivered message;
    /// `Err(io::Error)` is a transport-level failure.
    async fn recv(&mut self) -> io::Result<Option<Message>>;

    /// Close the transport. Best-effort; errors are logged at the
    /// caller and not surfaced (the migration is already at a
    /// terminal state when close is called).
    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// In-memory transport pair used by the loopback test. Source-side
/// and target-side halves are returned from [`channel_pair`].
pub mod inmem {
    use super::*;
    use tokio::sync::mpsc;

    /// Half of an in-memory transport pair. Holds a sender to the
    /// peer's inbox and a receiver from its own inbox.
    pub struct InMemTransport {
        tx: mpsc::Sender<Message>,
        rx: mpsc::Receiver<Message>,
    }

    /// Build a connected pair. The first returned value is the
    /// source-side handle, the second is the target-side; either
    /// can be moved into a tokio task and used as a [`Transport`].
    pub fn channel_pair(capacity: usize) -> (InMemTransport, InMemTransport) {
        let (a_tx, a_rx) = mpsc::channel(capacity);
        let (b_tx, b_rx) = mpsc::channel(capacity);
        (
            InMemTransport { tx: b_tx, rx: a_rx },
            InMemTransport { tx: a_tx, rx: b_rx },
        )
    }

    #[async_trait]
    impl Transport for InMemTransport {
        async fn send(&mut self, msg: Message) -> io::Result<()> {
            self.tx
                .send(msg)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "peer transport dropped"))
        }

        async fn recv(&mut self) -> io::Result<Option<Message>> {
            Ok(self.rx.recv().await)
        }

        async fn close(&mut self) -> io::Result<()> {
            // Drop the sender so the peer's `recv` returns `None`.
            // We do this by replacing with a fresh closed channel.
            let (closed, _) = mpsc::channel(1);
            self.tx = closed;
            Ok(())
        }
    }
}
