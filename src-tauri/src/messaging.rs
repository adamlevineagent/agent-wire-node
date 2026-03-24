// Wire Node — Messaging Module
//
// Wire-specific messaging: market surface display, credit balance, hosting stats.
// Supports sending bug reports, reading incoming messages, and health checks.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    pub id: String,
    pub sender_type: String, // "system" | "admin" | "operator"
    pub subject: Option<String>,
    pub body: String,
    pub message_type: String, // "message" | "bug_report" | "announcement" | "market_update"
    pub read_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub dismissed_at: Option<String>,
}

fn default_status() -> String {
    "open".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeSettings {
    pub display_name: Option<String>,
    pub storage_cap_gb: Option<f64>,
    pub mesh_hosting_enabled: Option<bool>,
    pub auto_update_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub overall: String, // "healthy" | "warning" | "error"
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    pub status: String, // "ok" | "warning" | "error"
    pub message: String,
}

/// Market surface data received via heartbeat
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarketSurface {
    pub opportunities: Vec<HostingOpportunity>,
    pub credit_balance: f64,
    pub hosting_stats: HostingStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostingOpportunity {
    pub document_id: String,
    pub corpus_id: String,
    pub expected_pulls: u64,
    pub credit_rate: f64,
    pub size_bytes: u64,
    pub body_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostingStats {
    pub documents_hosted: u64,
    pub total_pulls_served: u64,
    pub credits_earned_today: f64,
    pub credits_earned_total: f64,
}

/// Fetch messages for this node from Wire API
pub async fn get_messages(
    api_url: &str,
    access_token: &str,
    node_id: &str,
) -> Result<Vec<WireMessage>, String> {
    let client = reqwest::Client::new();

    let url = format!(
        "{}/api/v1/node/{}/messages?dismissed=false&limit=50",
        api_url, node_id
    );

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch messages: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        // 404 = messaging endpoints don't exist yet — return empty
        if status == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!("Messages endpoint returned 404 — not yet available");
            return Ok(Vec::new());
        }
        return Err(format!("Messages API error: {}", status));
    }

    let messages: Vec<WireMessage> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse messages: {}", e))?;

    Ok(messages)
}

/// Send a message from this node (bug report, etc.)
pub async fn send_message(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    body: &str,
    message_type: &str,
    subject: Option<&str>,
    metadata: Option<serde_json::Value>,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    let mut payload = serde_json::json!({
        "node_id": node_id,
        "body": body,
        "message_type": message_type,
        "subject": subject,
    });

    if let Some(meta) = metadata {
        payload["metadata"] = meta;
    }

    let resp = client
        .post(&format!("{}/api/v1/node/messages", api_url))
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to send message: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        // 404 = messaging endpoints don't exist yet — silently succeed
        if status == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!("Send message endpoint returned 404 — not yet available");
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Send message failed ({}): {}", status, body));
    }

    Ok(())
}

/// Dismiss a message
pub async fn dismiss_message(
    api_url: &str,
    access_token: &str,
    message_id: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();

    let resp = client
        .post(&format!(
            "{}/api/v1/node/messages/{}/dismiss",
            api_url, message_id
        ))
        .header("Authorization", format!("Bearer {}", access_token))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to dismiss message: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        // 404 = messaging endpoints don't exist yet — silently succeed
        if status == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!("Dismiss message endpoint returned 404 — not yet available");
            return Ok(());
        }
        return Err(format!("Dismiss API error: {}", status));
    }

    Ok(())
}

/// Run self-diagnostics and return health status
pub async fn check_health(
    cache_dir: &std::path::Path,
    storage_cap_gb: f64,
    tunnel_url: Option<&str>,
    last_sync_at: Option<&str>,
) -> HealthStatus {
    let mut checks = Vec::new();

    // 1. Disk space
    let cache_size = crate::sync::get_cache_size(cache_dir).await;
    let cap_bytes = (storage_cap_gb * 1024.0 * 1024.0 * 1024.0) as u64;
    let usage_pct = if cap_bytes > 0 {
        (cache_size as f64 / cap_bytes as f64) * 100.0
    } else {
        0.0
    };

    if usage_pct > 95.0 {
        checks.push(HealthCheck {
            name: "Disk Space".into(),
            status: "error".into(),
            message: format!(
                "Cache {:.0}% full - consider increasing storage cap",
                usage_pct
            ),
        });
    } else if usage_pct > 80.0 {
        checks.push(HealthCheck {
            name: "Disk Space".into(),
            status: "warning".into(),
            message: format!("Cache {:.0}% full", usage_pct),
        });
    } else {
        checks.push(HealthCheck {
            name: "Disk Space".into(),
            status: "ok".into(),
            message: format!("Cache {:.0}% full - healthy", usage_pct),
        });
    }

    // 2. Tunnel reachability
    if let Some(url) = tunnel_url {
        match reqwest::Client::new()
            .get(&format!("{}/health", url))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                checks.push(HealthCheck {
                    name: "Tunnel".into(),
                    status: "ok".into(),
                    message: "Tunnel reachable".into(),
                });
            }
            _ => {
                checks.push(HealthCheck {
                    name: "Tunnel".into(),
                    status: "error".into(),
                    message: "Tunnel unreachable from outside".into(),
                });
            }
        }
    } else {
        checks.push(HealthCheck {
            name: "Tunnel".into(),
            status: "warning".into(),
            message: "No tunnel URL configured".into(),
        });
    }

    // 3. Sync freshness
    if let Some(sync_at) = last_sync_at {
        if let Ok(sync_time) = chrono::DateTime::parse_from_rfc3339(sync_at) {
            let age = chrono::Utc::now().signed_duration_since(sync_time);
            if age.num_hours() > 24 {
                checks.push(HealthCheck {
                    name: "Sync".into(),
                    status: "warning".into(),
                    message: format!("Last sync was {}h ago", age.num_hours()),
                });
            } else {
                checks.push(HealthCheck {
                    name: "Sync".into(),
                    status: "ok".into(),
                    message: format!("Last sync {}m ago", age.num_minutes()),
                });
            }
        }
    } else {
        checks.push(HealthCheck {
            name: "Sync".into(),
            status: "warning".into(),
            message: "No sync recorded yet".into(),
        });
    }

    let has_error = checks.iter().any(|c| c.status == "error");
    let has_warning = checks.iter().any(|c| c.status == "warning");
    let overall = if has_error {
        "error".into()
    } else if has_warning {
        "warning".into()
    } else {
        "healthy".into()
    };

    HealthStatus { overall, checks }
}

/// Collect system diagnostics for bug reports
pub fn collect_diagnostics(
    health: &HealthStatus,
    app_version: &str,
    tunnel_url: Option<&str>,
    node_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "app_version": app_version,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "os_family": std::env::consts::FAMILY,
        "tunnel_url": tunnel_url,
        "node_id": node_id,
        "health": {
            "overall": health.overall,
            "checks": health.checks.iter().map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "status": c.status,
                    "message": c.message,
                })
            }).collect::<Vec<_>>(),
        },
    })
}
