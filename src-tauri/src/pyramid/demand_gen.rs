// pyramid/demand_gen.rs — WS-DEMAND-GEN (Phase 3)
//
// Demand-driven L0 generation: when retrieval encounters questions whose
// answers don't exist in the pyramid, this module fires async jobs that
// generate fresh evidence-grounded content answering each sub-question.
//
// The actual chain execution is a STUB — WS-EM-CHAIN owns the chain YAML.
// This module provides the infrastructure: job tracking, async dispatch,
// status polling, and event emission.

use std::sync::Arc;

use anyhow::Result;

use super::db;
use super::event_bus::{TaggedBuildEvent, TaggedKind};
use super::lock_manager::LockManager;
use super::PyramidState;

/// Execute a demand-gen job: load from DB, acquire write lock, run chain stubs
/// for each sub-question, emit events, and update job status.
///
/// Called inside `tokio::spawn` from the HTTP handler so the request returns
/// 202 immediately. The write lock serializes with builds, deltas, stale
/// refresh, and other demand-gen jobs on the same slug.
pub async fn execute_demand_gen(
    state: &Arc<PyramidState>,
    slug: &str,
    job_id: &str,
) -> Result<Vec<String>> {
    // 1. Load job from DB
    let job = {
        let conn = state.writer.lock().await;
        db::get_demand_gen_job(&conn, job_id)?
            .ok_or_else(|| anyhow::anyhow!("demand-gen job {job_id} not found"))?
    };

    // 2. Mark as running + emit DemandGenStarted
    {
        let conn = state.writer.lock().await;
        db::mark_demand_gen_running(&conn, job_id)?;
    }
    let _ = state.build_event_bus.tx.send(TaggedBuildEvent {
        slug: slug.to_string(),
        kind: TaggedKind::DemandGenStarted {
            sub_question: job.question.clone(),
            job_id: job_id.to_string(),
        },
    });

    // 3. Acquire write lock on slug (serializes with builds / deltas / stale refresh)
    let _slug_write_guard = LockManager::global().write(slug).await;

    // 4. STUB: For each sub-question, generate evidence-grounded L0 nodes.
    //    WS-EM-CHAIN will replace this stub with actual chain invocation via
    //    invoke_chain or build_runner. For now, we just record that each
    //    sub-question was "processed" and return empty node IDs.
    let generated_node_ids: Vec<String> = Vec::new();

    // In the future, this loop will:
    //   for sub_q in &job.sub_questions {
    //       let node_id = invoke_chain(state, slug, sub_q, ...).await?;
    //       generated_node_ids.push(node_id);
    //   }
    // For now the stub succeeds with no generated nodes — the infrastructure
    // (job lifecycle, locking, events, polling) is the deliverable.

    // 5. Mark complete + emit DemandGenCompleted
    {
        let conn = state.writer.lock().await;
        db::mark_demand_gen_complete(&conn, job_id, &generated_node_ids)?;
    }
    let _ = state.build_event_bus.tx.send(TaggedBuildEvent {
        slug: slug.to_string(),
        kind: TaggedKind::DemandGenCompleted {
            job_id: job_id.to_string(),
            new_node_ids: generated_node_ids.clone(),
        },
    });

    Ok(generated_node_ids)
}

/// Spawn a demand-gen job as a background task. Returns immediately so the
/// HTTP handler can return 202. On failure, the job is marked failed in DB.
pub fn spawn_demand_gen(state: Arc<PyramidState>, slug: String, job_id: String) {
    tokio::spawn(async move {
        let result = execute_demand_gen(&state, &slug, &job_id).await;
        if let Err(e) = &result {
            tracing::error!(
                slug = %slug,
                job_id = %job_id,
                error = %e,
                "demand-gen job failed"
            );
            // Best-effort: mark the job as failed in DB
            if let Ok(conn) = state.writer.try_lock() {
                let _ = db::mark_demand_gen_failed(&conn, &job_id, &e.to_string());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyramid::db::init_pyramid_db;
    use crate::pyramid::types::DemandGenJob;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_pyramid_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_create_and_get_job() {
        let conn = setup_db();
        let job = DemandGenJob {
            id: 0,
            job_id: "test-job-001".to_string(),
            slug: "test-slug".to_string(),
            question: "What is the architecture?".to_string(),
            sub_questions: vec![
                "What are the main components?".to_string(),
                "How do they interact?".to_string(),
            ],
            status: "queued".to_string(),
            result_node_ids: vec![],
            error_message: None,
            requested_at: "2026-04-08T12:00:00".to_string(),
            started_at: None,
            completed_at: None,
        };
        db::create_demand_gen_job(&conn, &job).unwrap();

        let fetched = db::get_demand_gen_job(&conn, "test-job-001")
            .unwrap()
            .expect("job should exist");
        assert_eq!(fetched.job_id, "test-job-001");
        assert_eq!(fetched.slug, "test-slug");
        assert_eq!(fetched.question, "What is the architecture?");
        assert_eq!(fetched.sub_questions.len(), 2);
        assert_eq!(fetched.status, "queued");
        assert!(fetched.result_node_ids.is_empty());
        assert!(fetched.error_message.is_none());
    }

    #[test]
    fn test_state_transitions_running_complete_failed() {
        let conn = setup_db();

        // Create two jobs
        let job1 = DemandGenJob {
            id: 0,
            job_id: "job-transition-1".to_string(),
            slug: "test-slug".to_string(),
            question: "Q1".to_string(),
            sub_questions: vec!["sub-1".to_string()],
            status: "queued".to_string(),
            result_node_ids: vec![],
            error_message: None,
            requested_at: "2026-04-08T12:00:00".to_string(),
            started_at: None,
            completed_at: None,
        };
        let job2 = DemandGenJob {
            id: 0,
            job_id: "job-transition-2".to_string(),
            slug: "test-slug".to_string(),
            question: "Q2".to_string(),
            sub_questions: vec![],
            status: "queued".to_string(),
            result_node_ids: vec![],
            error_message: None,
            requested_at: "2026-04-08T12:01:00".to_string(),
            started_at: None,
            completed_at: None,
        };
        db::create_demand_gen_job(&conn, &job1).unwrap();
        db::create_demand_gen_job(&conn, &job2).unwrap();

        // Job 1: queued -> running -> complete
        db::mark_demand_gen_running(&conn, "job-transition-1").unwrap();
        let j = db::get_demand_gen_job(&conn, "job-transition-1")
            .unwrap()
            .unwrap();
        assert_eq!(j.status, "running");
        assert!(j.started_at.is_some());

        let node_ids = vec!["node-a".to_string(), "node-b".to_string()];
        db::mark_demand_gen_complete(&conn, "job-transition-1", &node_ids).unwrap();
        let j = db::get_demand_gen_job(&conn, "job-transition-1")
            .unwrap()
            .unwrap();
        assert_eq!(j.status, "complete");
        assert_eq!(j.result_node_ids, vec!["node-a", "node-b"]);
        assert!(j.completed_at.is_some());

        // Job 2: queued -> running -> failed
        db::mark_demand_gen_running(&conn, "job-transition-2").unwrap();
        db::mark_demand_gen_failed(&conn, "job-transition-2", "chain execution timeout").unwrap();
        let j = db::get_demand_gen_job(&conn, "job-transition-2")
            .unwrap()
            .unwrap();
        assert_eq!(j.status, "failed");
        assert_eq!(j.error_message.as_deref(), Some("chain execution timeout"));
        assert!(j.completed_at.is_some());

        // Verify invalid transitions fail
        assert!(db::mark_demand_gen_running(&conn, "job-transition-1").is_err());
        assert!(db::mark_demand_gen_complete(&conn, "job-transition-2", &[]).is_err());
    }

    #[test]
    fn test_pending_and_list_jobs() {
        let conn = setup_db();

        // Create jobs in different states
        for (id, status) in [("j1", "queued"), ("j2", "queued"), ("j3", "queued")] {
            let job = DemandGenJob {
                id: 0,
                job_id: id.to_string(),
                slug: "my-slug".to_string(),
                question: format!("Question for {id}"),
                sub_questions: vec![],
                status: status.to_string(),
                result_node_ids: vec![],
                error_message: None,
                requested_at: "2026-04-08T12:00:00".to_string(),
                started_at: None,
                completed_at: None,
            };
            db::create_demand_gen_job(&conn, &job).unwrap();
        }

        // Move j2 to running, j3 to complete
        db::mark_demand_gen_running(&conn, "j2").unwrap();
        db::mark_demand_gen_running(&conn, "j3").unwrap();
        db::mark_demand_gen_complete(&conn, "j3", &["node-x".to_string()]).unwrap();

        // Pending = queued + running
        let pending = db::get_pending_demand_gen_jobs(&conn, "my-slug").unwrap();
        assert_eq!(pending.len(), 2);
        let pending_ids: Vec<&str> = pending.iter().map(|j| j.job_id.as_str()).collect();
        assert!(pending_ids.contains(&"j1"));
        assert!(pending_ids.contains(&"j2"));

        // List all = all 3 (most recent first)
        let all = db::list_demand_gen_jobs(&conn, "my-slug", 100).unwrap();
        assert_eq!(all.len(), 3);

        // List with limit
        let limited = db::list_demand_gen_jobs(&conn, "my-slug", 2).unwrap();
        assert_eq!(limited.len(), 2);

        // Wrong slug returns empty
        let empty = db::get_pending_demand_gen_jobs(&conn, "other-slug").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_allow_demand_gen_creates_job() {
        // This test verifies the job creation path that the HTTP handler will use:
        // create a job with status "queued", verify it exists and has correct fields.
        let conn = setup_db();
        let job_id = uuid::Uuid::new_v4().to_string();
        let job = DemandGenJob {
            id: 0,
            job_id: job_id.clone(),
            slug: "demand-slug".to_string(),
            question: "How does the delta chain work?".to_string(),
            sub_questions: vec![
                "What triggers a delta?".to_string(),
                "How are deltas applied?".to_string(),
                "What happens on conflict?".to_string(),
            ],
            status: "queued".to_string(),
            result_node_ids: vec![],
            error_message: None,
            requested_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            started_at: None,
            completed_at: None,
        };
        db::create_demand_gen_job(&conn, &job).unwrap();

        // Poll-style check: job exists and is queryable
        let polled = db::get_demand_gen_job(&conn, &job_id)
            .unwrap()
            .expect("job should be poll-able");
        assert_eq!(polled.status, "queued");
        assert_eq!(polled.sub_questions.len(), 3);
        assert_eq!(polled.slug, "demand-slug");

        // Verify it appears in pending list
        let pending = db::get_pending_demand_gen_jobs(&conn, "demand-slug").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, job_id);
    }
}
