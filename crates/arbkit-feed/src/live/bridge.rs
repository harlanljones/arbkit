//! Bridge from async feed tasks into the synchronous SPSC producer.
//!
//! Feed tasks push into an unbounded std channel; a dedicated bridge thread
//! drains into `try_push` on the engine ring without blocking the hot path.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::event::FeedEvent;

/// Sink for parsed feed events from async WebSocket tasks.
pub trait FeedEventSender: Send + Sync {
    /// Enqueue one event. Returns `false` when the downstream is closed.
    fn send(&self, event: FeedEvent) -> bool;
}

/// Receiver side for the bridge thread.
pub struct FeedEventReceiver {
    rx: Receiver<FeedEvent>,
}

impl FeedEventReceiver {
    /// Non-blocking drain of all currently queued events.
    pub fn try_drain(&mut self, out: &mut Vec<FeedEvent>) {
        while let Ok(event) = self.rx.try_recv() {
            out.push(event);
        }
    }

    /// Blocking receive with timeout.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<FeedEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

/// MPSC bridge between tokio feed tasks and the sync ring producer.
pub struct MpscFeedBridge {
    tx: Sender<FeedEvent>,
    rx: Receiver<FeedEvent>,
}

impl MpscFeedBridge {
    /// Create a connected sender/receiver pair.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx }
    }

    /// Producer handle for async feed tasks.
    pub fn sender(&self) -> MpscSender {
        MpscSender {
            tx: self.tx.clone(),
        }
    }

    /// Consumer handle for the bridge thread.
    pub fn receiver(self) -> FeedEventReceiver {
        FeedEventReceiver { rx: self.rx }
    }
}

impl Default for MpscFeedBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience constructor returning `(sender, receiver)`.
pub fn crossbeam_bridge() -> (MpscSender, FeedEventReceiver) {
    let bridge = MpscFeedBridge::new();
    let sender = bridge.sender();
    let receiver = bridge.receiver();
    (sender, receiver)
}

/// Cloneable sender wrapping the std channel.
#[derive(Clone)]
pub struct MpscSender {
    tx: Sender<FeedEvent>,
}

impl FeedEventSender for MpscSender {
    fn send(&self, event: FeedEvent) -> bool {
        self.tx.send(event).is_ok()
    }
}

/// Spawns a thread that drains `receiver` and pushes into `try_push` until
/// `running` is cleared.
#[allow(dead_code)]
pub fn spawn_ring_bridge<P, S>(
    mut receiver: FeedEventReceiver,
    mut try_push: P,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()>
where
    P: FnMut(FeedEvent) -> Result<(), S> + Send + 'static,
    S: Send + 'static,
{
    thread::Builder::new()
        .name("feed-ring-bridge".into())
        .spawn(move || {
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(event) = receiver.recv_timeout(Duration::from_millis(5)) {
                    while try_push(event).is_err() {
                        if !running.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        std::hint::spin_loop();
                    }
                }
            }
            let mut pending = Vec::new();
            receiver.try_drain(&mut pending);
            for event in pending {
                let _ = try_push(event);
            }
        })
        .expect("feed ring bridge spawns")
}
