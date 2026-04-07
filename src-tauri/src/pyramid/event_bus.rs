use crate::pyramid::types::{BuildProgress, BuildProgressV2};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone, Serialize)]
pub struct TaggedBuildEvent {
    pub slug: String,
    pub kind: TaggedKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaggedKind {
    Progress { done: i64, total: i64 },
    V2Snapshot(BuildProgressV2),
    Resync,
}

pub struct BuildEventBus {
    pub tx: broadcast::Sender<TaggedBuildEvent>,
}

impl BuildEventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(4096);
        Self { tx }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TaggedBuildEvent> {
        self.tx.subscribe()
    }
}

impl Default for BuildEventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Creates an mpsc channel for BuildProgress AND spawns a relay task that
/// forwards every event onto the broadcast bus tagged with the given slug.
/// Per v3.3 B3 — call this at every build-launch site that previously did
/// `mpsc::channel::<BuildProgress>(64)` directly.
///
/// NOTE: this helper consumes the receiver internally, so it is only suitable
/// for sites that do not need a downstream consumer of the BuildProgress
/// stream. Sites that already have a desktop UI consumer reading from the
/// receiver should use [`tee_build_progress_to_bus`] instead.
pub fn spawn_build_progress_channel(
    bus: &BuildEventBus,
    slug: String,
) -> mpsc::Sender<BuildProgress> {
    let (tx, mut rx) = mpsc::channel::<BuildProgress>(256);
    let bus_tx = bus.tx.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = bus_tx.send(TaggedBuildEvent {
                slug: slug.clone(),
                kind: TaggedKind::Progress {
                    done: p.done,
                    total: p.total,
                },
            });
        }
    });
    tx
}

/// Tee variant: takes ownership of an existing upstream `Receiver<BuildProgress>`,
/// spawns a relay task that forwards every event onto the broadcast bus tagged
/// with `slug`, AND returns a downstream receiver that yields the same events.
///
/// This is the minimum-friction substitution for build-launch sites that
/// previously did `let (tx, rx) = mpsc::channel(64)` and then read from `rx`
/// to drive the desktop UI / build status. Replace with:
///
/// ```ignore
/// let (progress_tx, raw_rx) = tokio::sync::mpsc::channel::<BuildProgress>(64);
/// let mut progress_rx = crate::pyramid::event_bus::tee_build_progress_to_bus(
///     &state.build_event_bus,
///     slug.clone(),
///     raw_rx,
/// );
/// ```
///
/// The desktop UI consumer continues reading from `progress_rx` exactly as
/// before; the bus tee is purely additive.
pub fn tee_build_progress_to_bus(
    bus: &BuildEventBus,
    slug: String,
    upstream: mpsc::Receiver<BuildProgress>,
) -> mpsc::Receiver<BuildProgress> {
    let (down_tx, down_rx) = mpsc::channel::<BuildProgress>(256);
    let bus_tx = bus.tx.clone();
    tokio::spawn(async move {
        let mut up = upstream;
        while let Some(p) = up.recv().await {
            // Mirror onto the broadcast bus first (lossy/best-effort).
            let _ = bus_tx.send(TaggedBuildEvent {
                slug: slug.clone(),
                kind: TaggedKind::Progress {
                    done: p.done,
                    total: p.total,
                },
            });
            // Then forward to the downstream consumer. If the downstream
            // consumer has dropped its receiver, stop relaying.
            if down_tx.send(p).await.is_err() {
                break;
            }
        }
    });
    down_rx
}
