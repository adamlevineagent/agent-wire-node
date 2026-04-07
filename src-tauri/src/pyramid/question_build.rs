//! Lib-side runner for question-pyramid builds.
//!
//! Extracted from `main.rs::pyramid_question_build_inner` so the public web
//! surface (`routes_ask`) can spawn a question build without going through
//! the Tauri IPC layer. The Tauri IPC command in `main.rs` is now a thin
//! wrapper that calls into here.
//!
//! Behavior: validates the slug exists, registers a `BuildHandle` in
//! `state.active_build`, spawns a tokio task that runs
//! `build_runner::run_decomposed_build` with progress tee'd through
//! `state.build_event_bus`, and returns immediately with a "started" JSON
//! envelope. The caller does NOT await build completion — progress is
//! observed via the per-slug WebSocket.

use std::sync::Arc;

use crate::pyramid::types::{
    BuildLayerState, BuildProgress, BuildStatus, CharacterizationResult, LayerEvent,
    LayerProgress, LogEntry, NodeStatus,
};
use crate::pyramid::{BuildHandle, PyramidState};

/// Spawn a decomposed question build for `slug`.
///
/// Returns immediately after registering the build handle and spawning the
/// background task. Returns `Err` if the slug does not exist, the question is
/// empty, or another build is already running for the slug.
pub async fn spawn_question_build(
    state: &Arc<PyramidState>,
    slug: String,
    question: String,
    granularity: u32,
    max_depth: u32,
    from_depth: i64,
    characterization: Option<CharacterizationResult>,
) -> Result<serde_json::Value, String> {
    if question.trim().is_empty() {
        return Err("question cannot be empty".to_string());
    }

    // Validate slug exists.
    {
        let conn = state.reader.lock().await;
        crate::pyramid::slug::get_slug(&conn, &slug)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Slug '{}' not found", slug))?;
    }

    let cancel = tokio_util::sync::CancellationToken::new();
    let status = Arc::new(tokio::sync::RwLock::new(BuildStatus {
        slug: slug.clone(),
        status: "running".to_string(),
        progress: BuildProgress { done: 0, total: 0 },
        elapsed_seconds: 0.0,
        failures: 0,
        steps: vec![],
    }));

    let layer_state_for_build = {
        let mut active = state.active_build.write().await;
        if let Some(handle) = active.get(&slug) {
            let s = handle.status.read().await;
            let is_terminal = s.is_terminal();
            drop(s);
            if !handle.cancel.is_cancelled() && !is_terminal {
                return Err("Build already running for this slug".to_string());
            }
        }

        let layer_state = Arc::new(tokio::sync::RwLock::new(BuildLayerState::default()));
        let layer_state_for_build = layer_state.clone();
        let handle = BuildHandle {
            slug: slug.clone(),
            cancel: cancel.clone(),
            status: status.clone(),
            layer_state,
            started_at: std::time::Instant::now(),
        };
        active.insert(slug.clone(), handle);
        layer_state_for_build
    };

    // Build runs against its own reader connection so it doesn't compete with
    // CLI/frontend queries for the shared reader Mutex.
    let pyramid_state = state
        .with_build_reader()
        .map_err(|e| format!("Failed to create build reader: {e}"))?;
    let build_status = status.clone();
    let build_question = question.clone();
    let build_slug = slug.clone();

    let question_build_handle = tokio::spawn(async move {
        let start = std::time::Instant::now();

        let (progress_tx, raw_progress_rx) = tokio::sync::mpsc::channel::<BuildProgress>(64);
        let mut progress_rx = crate::pyramid::event_bus::tee_build_progress_to_bus(
            &pyramid_state.build_event_bus,
            build_slug.clone(),
            raw_progress_rx,
        );
        let progress_status = build_status.clone();
        let progress_start = start;
        let progress_handle = tokio::spawn(async move {
            while let Some(prog) = progress_rx.recv().await {
                let mut s = progress_status.write().await;
                s.progress = prog;
                s.elapsed_seconds = progress_start.elapsed().as_secs_f64();
            }
        });

        let (layer_tx, mut layer_rx) = tokio::sync::mpsc::channel::<LayerEvent>(256);
        let layer_drain_state = layer_state_for_build;
        let layer_drain_handle = tokio::spawn(async move {
            while let Some(event) = layer_rx.recv().await {
                let mut state = layer_drain_state.write().await;
                match event {
                    LayerEvent::Discovered {
                        depth,
                        step_name,
                        estimated_nodes,
                    } => {
                        state.layers.push(LayerProgress {
                            depth,
                            step_name,
                            estimated_nodes,
                            completed_nodes: 0,
                            failed_nodes: 0,
                            status: "pending".into(),
                            nodes: if estimated_nodes <= 50 {
                                Some(Vec::new())
                            } else {
                                None
                            },
                        });
                    }
                    LayerEvent::NodeCompleted {
                        depth,
                        step_name,
                        node_id,
                        label,
                    } => {
                        if let Some(layer) = state
                            .layers
                            .iter_mut()
                            .find(|l| l.depth == depth && l.step_name == step_name)
                        {
                            layer.completed_nodes += 1;
                            layer.status = "active".into();
                            if let Some(ref mut nodes) = layer.nodes {
                                nodes.push(NodeStatus {
                                    node_id,
                                    status: "complete".into(),
                                    label,
                                });
                            }
                        }
                    }
                    LayerEvent::NodeFailed {
                        depth,
                        step_name,
                        node_id,
                    } => {
                        if let Some(layer) = state
                            .layers
                            .iter_mut()
                            .find(|l| l.depth == depth && l.step_name == step_name)
                        {
                            layer.failed_nodes += 1;
                            if let Some(ref mut nodes) = layer.nodes {
                                nodes.push(NodeStatus {
                                    node_id,
                                    status: "failed".into(),
                                    label: None,
                                });
                            }
                        }
                    }
                    LayerEvent::LayerCompleted { depth, step_name } => {
                        if let Some(layer) = state
                            .layers
                            .iter_mut()
                            .find(|l| l.depth == depth && l.step_name == step_name)
                        {
                            layer.status = "complete".into();
                        }
                    }
                    LayerEvent::NodeStarted {
                        depth,
                        step_name,
                        node_id,
                        ..
                    } => {
                        if let Some(layer) = state
                            .layers
                            .iter_mut()
                            .find(|l| l.depth == depth && l.step_name == step_name)
                        {
                            if let Some(ref mut nodes) = layer.nodes {
                                nodes.push(NodeStatus {
                                    node_id,
                                    status: "pending".into(),
                                    label: None,
                                });
                            }
                        }
                    }
                    LayerEvent::StepStarted { step_name } => {
                        state.current_step = Some(step_name);
                    }
                    LayerEvent::Log {
                        elapsed_secs,
                        message,
                    } => {
                        state.log.push_back(LogEntry {
                            elapsed_secs,
                            message,
                        });
                        if state.log.len() > 200 {
                            state.log.pop_front();
                        }
                    }
                }
            }
        });

        let result = crate::pyramid::build_runner::run_decomposed_build(
            &pyramid_state,
            &build_slug,
            &build_question,
            granularity,
            max_depth,
            from_depth,
            characterization,
            &cancel,
            Some(progress_tx.clone()),
            Some(layer_tx.clone()),
        )
        .await;

        drop(progress_tx);
        drop(layer_tx);
        let _ = progress_handle.await;
        let _ = layer_drain_handle.await;

        {
            let mut s = build_status.write().await;
            if cancel.is_cancelled() {
                s.status = "cancelled".to_string();
            } else {
                match result {
                    Ok((_apex, failures, activities)) => {
                        if failures > 0 {
                            s.status = "complete_with_errors".to_string();
                        } else {
                            s.status = "complete".to_string();
                        }
                        s.failures = failures;
                        s.steps = activities;
                    }
                    Err(e) => {
                        tracing::error!(slug = %build_slug, error = %e, "question build failed");
                        s.status = "failed".to_string();
                        s.failures = -1;
                    }
                }
            }
            s.elapsed_seconds = start.elapsed().as_secs_f64();
        }
    });

    // Monitor: catch panics in the build task.
    let monitor_status = status.clone();
    tokio::spawn(async move {
        if let Err(e) = question_build_handle.await {
            tracing::error!("pyramid_question_build task panicked: {e:?}");
            let mut s = monitor_status.write().await;
            if s.status == "running" {
                s.status = "failed".to_string();
            }
        }
    });

    Ok(serde_json::json!({
        "status": "started",
        "slug": slug,
        "build_type": "question_decomposition",
        "question": question,
        "granularity": granularity,
        "max_depth": max_depth,
        "from_depth": from_depth,
    }))
}
