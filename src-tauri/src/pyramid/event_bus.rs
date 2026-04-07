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
