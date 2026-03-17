// Wire Node — Cloudflare Tunnel Management
//
// Handles:
//   - Downloading cloudflared binary if not present
//   - Provisioning a tunnel via the server-side API
//   - Running cloudflared as a child process
//   - Monitoring tunnel health
//   - Persisting tunnel credentials locally

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Tunnel state stored locally
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TunnelState {
    pub tunnel_id: Option<String>,
    pub tunnel_url: Option<String>,
    pub tunnel_token: Option<String>,
    pub status: TunnelConnectionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TunnelConnectionStatus {
    Disconnected,
    Provisioning,
    Downloading,
    Connecting,
    Connected,
    Error(String),
}

impl Default for TunnelConnectionStatus {
    fn default() -> Self {
        TunnelConnectionStatus::Disconnected
    }
}

/// Response from POST /api/relay/tunnel (or Wire equivalent)
#[derive(Debug, Deserialize)]
struct ProvisionResponse {
    tunnel_token: String,
    tunnel_url: String,
    tunnel_id: String,
    #[allow(dead_code)]
    existing: bool,
}

// --- Binary Management ------------------------------------------------------

/// Get the path where cloudflared binary should be stored
fn cloudflared_binary_path(data_dir: &Path) -> PathBuf {
    let binary_name = if cfg!(target_os = "windows") {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };
    data_dir.join("bin").join(binary_name)
}

/// Check if cloudflared binary exists
pub fn is_cloudflared_installed(data_dir: &Path) -> bool {
    cloudflared_binary_path(data_dir).exists()
}

/// Download cloudflared binary from GitHub releases
pub async fn download_cloudflared(data_dir: &Path) -> Result<PathBuf, String> {
    let binary_path = cloudflared_binary_path(data_dir);

    if binary_path.exists() {
        #[cfg(unix)]
        {
            let output = std::process::Command::new(&binary_path)
                .arg("--version")
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    tracing::info!("cloudflared already installed at {:?}", binary_path);
                    return Ok(binary_path);
                }
                _ => {
                    tracing::warn!("cloudflared exists but isn't runnable - re-downloading");
                    let _ = std::fs::remove_file(&binary_path);
                }
            }
        }
        #[cfg(not(unix))]
        {
            tracing::info!("cloudflared already installed at {:?}", binary_path);
            return Ok(binary_path);
        }
    }

    let download_url = get_cloudflared_download_url()?;
    let is_tgz = download_url.ends_with(".tgz");

    tracing::info!("Downloading cloudflared from {}", download_url);

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let resp = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("Failed to download cloudflared: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Download failed with status: {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download: {}", e))?;

    let bin_dir = binary_path.parent().unwrap();
    tokio::fs::create_dir_all(bin_dir)
        .await
        .map_err(|e| format!("Failed to create bin dir: {}", e))?;

    if is_tgz {
        let tgz_path = bin_dir.join("cloudflared.tgz");
        tokio::fs::write(&tgz_path, &bytes)
            .await
            .map_err(|e| format!("Failed to write cloudflared.tgz: {}", e))?;

        let output = std::process::Command::new("tar")
            .args(["xzf", "cloudflared.tgz"])
            .current_dir(bin_dir)
            .output()
            .map_err(|e| format!("Failed to extract cloudflared.tgz: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tar extraction failed: {}", stderr));
        }

        let _ = std::fs::remove_file(&tgz_path);
        tracing::info!("cloudflared extracted from .tgz at {:?}", binary_path);
    } else {
        tokio::fs::write(&binary_path, &bytes)
            .await
            .map_err(|e| format!("Failed to write cloudflared binary: {}", e))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&binary_path, perms)
            .map_err(|e| format!("Failed to set permissions: {}", e))?;
    }

    tracing::info!("cloudflared installed at {:?}", binary_path);
    Ok(binary_path)
}

fn get_cloudflared_download_url() -> Result<String, String> {
    let base = "https://github.com/cloudflare/cloudflared/releases/latest/download";

    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            Ok(format!("{}/cloudflared-darwin-arm64.tgz", base))
        } else {
            Ok(format!("{}/cloudflared-darwin-amd64.tgz", base))
        }
    } else if cfg!(target_os = "windows") {
        Ok(format!("{}/cloudflared-windows-amd64.exe", base))
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            Ok(format!("{}/cloudflared-linux-arm64", base))
        } else {
            Ok(format!("{}/cloudflared-linux-amd64", base))
        }
    } else {
        Err("Unsupported platform".to_string())
    }
}

// --- Tunnel Provisioning ----------------------------------------------------

/// Provision a tunnel through the Wire server-side API
pub async fn provision_tunnel(
    api_base_url: &str,
    access_token: &str,
    node_id: &str,
) -> Result<TunnelState, String> {
    let client = reqwest::Client::new();

    let url = format!("{}/api/v1/node/tunnel", api_base_url);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "node_id": node_id }))
        .send()
        .await
        .map_err(|e| format!("Tunnel provisioning request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Tunnel provisioning failed ({}): {}", status, text));
    }

    let provision: ProvisionResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse tunnel response: {}", e))?;

    tracing::info!(
        "Tunnel provisioned: {} ({})",
        provision.tunnel_url,
        if provision.existing { "existing" } else { "new" }
    );

    Ok(TunnelState {
        tunnel_id: Some(provision.tunnel_id),
        tunnel_url: Some(provision.tunnel_url),
        tunnel_token: Some(provision.tunnel_token),
        status: TunnelConnectionStatus::Provisioning,
    })
}

// --- Tunnel Process Management ----------------------------------------------

/// Start cloudflared tunnel run with the given token
pub async fn start_tunnel(
    data_dir: &Path,
    tunnel_token: &str,
) -> Result<Child, String> {
    let binary_path = cloudflared_binary_path(data_dir);

    if !binary_path.exists() {
        return Err("cloudflared binary not found - call download_cloudflared first".to_string());
    }

    // Kill any orphan cloudflared processes
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg("cloudflared tunnel run")
            .output();
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "cloudflared.exe"])
            .output();
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    tracing::info!("Starting cloudflared tunnel...");

    let actual_binary = if cfg!(target_os = "macos") {
        let extracted_path = data_dir.join("bin").join("cloudflared");
        if extracted_path.exists() {
            extracted_path
        } else {
            binary_path
        }
    } else {
        binary_path
    };

    let child = Command::new(&actual_binary)
        .arg("tunnel")
        .arg("run")
        .arg("--token")
        .arg(tunnel_token)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to start cloudflared: {}", e))?;

    tracing::info!("cloudflared started (pid: {:?})", child.id());

    Ok(child)
}

/// Monitor cloudflared stderr for connection status.
pub async fn monitor_tunnel_output(child: &mut Child) -> TunnelConnectionStatus {
    if let Some(stderr) = child.stderr.take() {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        let mut connected = false;
        for _ in 0..50 {
            match tokio::time::timeout(
                tokio::time::Duration::from_secs(10),
                lines.next_line(),
            ).await {
                Ok(Ok(Some(line))) => {
                    tracing::debug!("cloudflared: {}", line);

                    if line.contains("Registered tunnel connection") ||
                       line.contains("Connection registered") ||
                       line.contains("connIndex=") {
                        connected = true;
                        break;
                    }

                    let lower = line.to_lowercase();
                    let is_benign =
                        lower.contains("failed to sufficiently") ||
                        lower.contains("update check") ||
                        lower.contains("buffer size") ||
                        lower.contains("metrics server") ||
                        lower.contains("capacity") ||
                        (lower.contains(" inf ") && !lower.contains("tunnel connection failed"));

                    if !is_benign && (
                        lower.contains(" err ") ||
                        lower.contains("\"level\":\"error\"") ||
                        lower.contains("failed to connect to edge") ||
                        lower.contains("tunnel connection failed") ||
                        lower.contains("authentication failed") ||
                        lower.contains("credential") && lower.contains("error")
                    ) {
                        tracing::warn!("cloudflared error: {}", line);
                        tokio::spawn(async move {
                            while let Ok(Some(_)) = lines.next_line().await {}
                        });
                        return TunnelConnectionStatus::Error(line);
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    return TunnelConnectionStatus::Error(format!("Read error: {}", e));
                }
                Err(_) => break,
            }
        }

        // Keep draining stderr in background to prevent SIGPIPE
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::trace!("cloudflared: {}", line);
            }
        });

        if connected {
            TunnelConnectionStatus::Connected
        } else {
            TunnelConnectionStatus::Connecting
        }
    } else {
        TunnelConnectionStatus::Error("No stderr available".to_string())
    }
}

// --- Persistence ------------------------------------------------------------

/// Save tunnel state to disk
pub fn save_tunnel_state(data_dir: &Path, state: &TunnelState) {
    let path = data_dir.join("tunnel.json");
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
        tracing::debug!("Tunnel state saved");
    }
}

/// Load tunnel state from disk
pub fn load_tunnel_state(data_dir: &Path) -> Option<TunnelState> {
    let path = data_dir.join("tunnel.json");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}
