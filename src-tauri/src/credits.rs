use serde::{Deserialize, Serialize};
use chrono::Utc;

/// Tracks document pulls served and credits earned
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct CreditTracker {
    pub documents_served: u64,
    pub pulls_served_total: u64,
    pub credits_earned: u64,
    pub total_bytes_served: u64,
    pub session_documents_served: u64,
    pub session_bytes_served: u64,
    pub today_documents_served: u64,
    pub today_bytes_served: u64,
    pub today_date: Option<String>,
    pub recent_events: Vec<ServeEvent>,
    pub session_started_at: Option<String>,
    pub first_started_at: Option<String>,
    pub total_uptime_seconds: u64,
    pub total_unique_consumers: u64,
    #[serde(default)]
    pub server_credit_balance: f64,

    // Achievement-synced counters (updated from WorkStats / MarketState each tick)
    #[serde(default)]
    pub total_jobs_completed: u64,
    #[serde(default)]
    pub documents_hosted: u64,
    #[serde(default)]
    pub bytes_hosted: u64,
    #[serde(default)]
    pub retention_challenges_passed: u64,
    #[serde(default)]
    pub unique_corpora_hosted: u64,

    // Batched serve log — accumulated between reports, flushed every 60s
    #[serde(skip)]
    pub pending_serve_log: Vec<ServeLogEntry>,
    // Delta counters — accumulated between reports
    #[serde(skip)]
    pub delta_documents_served: u64,
    #[serde(skip)]
    pub delta_bytes_served: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeEvent {
    pub document_id: String,
    pub bytes: u64,
    pub timestamp: String,
    pub message: String,
    pub token_id: String,
    #[serde(default = "default_event_type")]
    pub event_type: String,
}

fn default_event_type() -> String {
    "serve".to_string()
}

/// Entry for batched serve reporting — matches server's expected format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeLogEntry {
    pub document_id: String,
    pub token_id: String,
    pub consumer_operator_id: String,
    pub served_at: String,
}

const MAX_RECENT_EVENTS: usize = 10_000;

// --- Achievement System (adapted for Wire economy) --------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Achievement {
    pub id: String,
    pub emoji: String,
    pub current_level: u32,
    pub current_name: String,
    pub next_name: Option<String>,
    pub next_threshold: Option<u64>,
    pub current_value: u64,
    pub progress_pct: f64,
}

struct AchievementTrack {
    id: &'static str,
    emoji: &'static str,
    levels: &'static [(&'static str, u64)],
}

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;
const TB: u64 = 1024 * GB;
const DAY: u64 = 24 * 3600;

const TRACKS: &[AchievementTrack] = &[
    // 1. The Wire — pulls served (signal distribution)
    AchievementTrack {
        id: "pulls_served",
        emoji: "\u{1F4E1}",  // 📡
        levels: &[
            ("Tipster", 1),
            ("Stringer", 25),
            ("Reporter", 100),
            ("Correspondent", 500),
            ("Bureau Chief", 2_500),
            ("Editor", 10_000),
            ("Syndicate", 50_000),
            ("Wire Service", 250_000),
            ("Press Empire", 1_000_000),
            ("Voice of Record", 10_000_000),
        ],
    },
    // 2. Bandwidth — bytes served (data moved)
    AchievementTrack {
        id: "bytes_served",
        emoji: "\u{1F4BE}",  // 💾
        levels: &[
            ("First Byte", 1 * MB),
            ("Packet Runner", 100 * MB),
            ("Megabyte Mark", 500 * MB),
            ("Gigabyte Club", 1 * GB),
            ("Heavy Lifter", 10 * GB),
            ("Freight Line", 50 * GB),
            ("Data Mountain", 250 * GB),
            ("Terabyte Club", 1 * TB),
            ("Petabyte Path", 10 * TB),
            ("Backbone", 100 * TB),
        ],
    },
    // 3. On The Beat — total uptime (always available)
    AchievementTrack {
        id: "uptime",
        emoji: "\u{23F1}",  // ⏱
        levels: &[
            ("First Shift", 1 * DAY),
            ("Night Owl", 3 * DAY),
            ("Week Strong", 7 * DAY),
            ("Deadline Keeper", 14 * DAY),
            ("Monthly", 30 * DAY),
            ("Old Reliable", 90 * DAY),
            ("Half Year", 182 * DAY),
            ("Annual", 365 * DAY),
            ("Veteran", 730 * DAY),
            ("Lifer", 1825 * DAY),
        ],
    },
    // 4. Follow The Money — credits earned
    AchievementTrack {
        id: "credits_earned",
        emoji: "\u{1F4B0}",  // 💰
        levels: &[
            ("Penny Press", 10),
            ("Tip Jar", 100),
            ("Side Hustle", 500),
            ("Funded", 2_500),
            ("Bankrolled", 10_000),
            ("Syndicated", 50_000),
            ("Wire Transfer", 250_000),
            ("Deep Pockets", 1_000_000),
            ("Endowed", 10_000_000),
            ("The Mint", 100_000_000),
        ],
    },
    // 5. The Grind — jobs completed (work engine)
    AchievementTrack {
        id: "jobs_completed",
        emoji: "\u{1F527}",  // 🔧
        levels: &[
            ("Intern", 1),
            ("Copy Runner", 10),
            ("Desk Jockey", 50),
            ("Beat Worker", 250),
            ("Workhorse", 1_000),
            ("Machine", 5_000),
            ("Engine", 25_000),
            ("Powerplant", 100_000),
            ("Perpetual Motion", 500_000),
            ("Force of Nature", 5_000_000),
        ],
    },
    // 6. Mesh Weaver — documents hosted (market inventory count)
    AchievementTrack {
        id: "docs_hosted",
        emoji: "\u{1F578}",  // 🕸
        levels: &[
            ("First Pin", 1),
            ("Collector", 5),
            ("Curator", 25),
            ("Archive", 100),
            ("Repository", 500),
            ("Vault", 2_000),
            ("Citadel", 10_000),
            ("Fortress", 50_000),
            ("Atlas", 250_000),
            ("World Library", 1_000_000),
        ],
    },
    // 7. Reach — unique consumers served
    AchievementTrack {
        id: "unique_consumers",
        emoji: "\u{1F91D}",  // 🤝
        levels: &[
            ("First Contact", 1),
            ("Pen Pal", 5),
            ("Socialite", 25),
            ("Connector", 100),
            ("Hub", 500),
            ("Nexus", 2_000),
            ("Switchboard", 10_000),
            ("Exchange", 50_000),
            ("Gateway", 250_000),
            ("Grand Central", 1_000_000),
        ],
    },
    // 8. Integrity — retention challenges passed
    AchievementTrack {
        id: "retention",
        emoji: "\u{1F6E1}",  // 🛡
        levels: &[
            ("Tested", 1),
            ("Verified", 10),
            ("Proven", 50),
            ("Reliable", 250),
            ("Steadfast", 1_000),
            ("Ironclad", 5_000),
            ("Unshakeable", 25_000),
            ("Bulwark", 100_000),
            ("Bastion", 500_000),
            ("Unbreakable", 5_000_000),
        ],
    },
    // 9. Stockpile — bytes committed to mesh hosting
    AchievementTrack {
        id: "bytes_hosted",
        emoji: "\u{1F4E6}",  // 📦
        levels: &[
            ("Shelf Space", 100 * MB),
            ("Filing Cabinet", 500 * MB),
            ("Closet", 1 * GB),
            ("Warehouse", 5 * GB),
            ("Hangar", 25 * GB),
            ("Silo", 100 * GB),
            ("Bunker", 500 * GB),
            ("Data Center", 1 * TB),
            ("Server Farm", 10 * TB),
            ("The Cloud", 100 * TB),
        ],
    },
    // 10. Coverage — unique corpora hosted
    AchievementTrack {
        id: "corpora_hosted",
        emoji: "\u{1F4DA}",  // 📚
        levels: &[
            ("Niche", 1),
            ("Specialist", 3),
            ("Generalist", 10),
            ("Polymath", 25),
            ("Encyclopedia", 50),
            ("Omnivore", 100),
            ("Renaissance", 250),
            ("Universal", 500),
            ("Alexandrian", 1_000),
            ("Akashic Record", 5_000),
        ],
    },
];

fn compute_achievements(tracker: &CreditTracker) -> Vec<Achievement> {
    let values: Vec<(&str, u64)> = vec![
        ("pulls_served", tracker.pulls_served_total),
        ("bytes_served", tracker.total_bytes_served),
        ("uptime", tracker.total_uptime_seconds),
        ("credits_earned", tracker.credits_earned),
        ("jobs_completed", tracker.total_jobs_completed),
        ("docs_hosted", tracker.documents_hosted),
        ("unique_consumers", tracker.total_unique_consumers),
        ("retention", tracker.retention_challenges_passed),
        ("bytes_hosted", tracker.bytes_hosted),
        ("corpora_hosted", tracker.unique_corpora_hosted),
    ];

    TRACKS.iter().map(|track| {
        let current_value = values.iter()
            .find(|(id, _)| *id == track.id)
            .map(|(_, v)| *v)
            .unwrap_or(0);

        let mut current_level: u32 = 0;
        let mut current_name = String::new();
        for (i, (name, threshold)) in track.levels.iter().enumerate() {
            if current_value >= *threshold {
                current_level = (i + 1) as u32;
                current_name = name.to_string();
            }
        }

        let next_idx = current_level as usize;
        let (next_name, next_threshold) = if next_idx < track.levels.len() {
            let (name, thresh) = track.levels[next_idx];
            (Some(name.to_string()), Some(thresh))
        } else {
            (None, None)
        };

        let progress_pct = match next_threshold {
            Some(next_t) => {
                let prev_t = if current_level > 0 {
                    track.levels[(current_level - 1) as usize].1
                } else {
                    0
                };
                let range = next_t - prev_t;
                if range > 0 {
                    let progress = current_value.saturating_sub(prev_t);
                    (progress as f64 / range as f64 * 100.0).min(100.0)
                } else {
                    0.0
                }
            }
            None => 100.0,
        };

        Achievement {
            id: track.id.to_string(),
            emoji: track.emoji.to_string(),
            current_level,
            current_name: if current_level > 0 { current_name } else { "--".to_string() },
            next_name,
            next_threshold,
            current_value,
            progress_pct,
        }
    }).collect()
}

impl CreditTracker {
    pub fn init_session(&mut self) {
        self.session_started_at = Some(Utc::now().to_rfc3339());
        if self.first_started_at.is_none() {
            self.first_started_at = Some(Utc::now().to_rfc3339());
        }
    }

    /// Tick cumulative uptime by 60 seconds
    pub fn tick_uptime(&mut self) {
        self.total_uptime_seconds += 60;
    }

    /// Load persisted cumulative stats from disk
    pub fn load_from_file(path: &std::path::Path) -> Self {
        let data = match std::fs::read_to_string(path) {
            Ok(d) => d,
            Err(_) => return Self::default(),
        };
        let persisted: Self = match serde_json::from_str(&data) {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        Self {
            documents_served: persisted.documents_served,
            pulls_served_total: persisted.pulls_served_total,
            credits_earned: persisted.credits_earned,
            total_bytes_served: persisted.total_bytes_served,
            first_started_at: persisted.first_started_at,
            total_uptime_seconds: persisted.total_uptime_seconds,
            total_unique_consumers: persisted.total_unique_consumers,
            // Achievement counters (persisted across sessions)
            total_jobs_completed: persisted.total_jobs_completed,
            documents_hosted: persisted.documents_hosted,
            bytes_hosted: persisted.bytes_hosted,
            retention_challenges_passed: persisted.retention_challenges_passed,
            unique_corpora_hosted: persisted.unique_corpora_hosted,
            // Reset session-specific fields
            session_documents_served: 0,
            session_bytes_served: 0,
            session_started_at: None,
            today_documents_served: 0,
            today_bytes_served: 0,
            today_date: None,
            recent_events: Vec::new(),
            pending_serve_log: Vec::new(),
            delta_documents_served: 0,
            delta_bytes_served: 0,
            server_credit_balance: 0.0,
        }
    }

    /// Save current stats to disk
    pub fn save_to_file(&self, path: &std::path::Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Record a document serve event
    pub fn record_serve(&mut self, bytes: u64, document_id: &str, token_id: &str, consumer_operator_id: &str) {
        let now = Utc::now();
        let today = now.format("%Y-%m-%d").to_string();

        // Reset daily counters if new day
        if self.today_date.as_deref() != Some(&today) {
            self.today_documents_served = 0;
            self.today_bytes_served = 0;
            self.today_date = Some(today);
        }

        self.documents_served += 1;
        self.pulls_served_total += 1;
        self.total_bytes_served += bytes;
        self.session_documents_served += 1;
        self.session_bytes_served += bytes;
        self.today_documents_served += 1;
        self.today_bytes_served += bytes;

        // Delta tracking for batched reporting
        self.delta_documents_served += 1;
        self.delta_bytes_served += bytes;

        // Add to pending serve log for batched reporting
        self.pending_serve_log.push(ServeLogEntry {
            document_id: document_id.to_string(),
            token_id: token_id.to_string(),
            consumer_operator_id: consumer_operator_id.to_string(),
            served_at: now.to_rfc3339(),
        });

        let msg = format!("Document {} served ({} bytes)", document_id, bytes);

        let event = ServeEvent {
            document_id: document_id.to_string(),
            bytes,
            timestamp: now.to_rfc3339(),
            message: msg,
            token_id: token_id.to_string(),
            event_type: "serve".to_string(),
        };

        self.recent_events.insert(0, event);
        if self.recent_events.len() > MAX_RECENT_EVENTS {
            self.recent_events.truncate(MAX_RECENT_EVENTS);
        }
    }

    /// Take pending serve log entries for batched reporting, clearing the buffer
    pub fn take_pending_serves(&mut self) -> Vec<ServeLogEntry> {
        std::mem::take(&mut self.pending_serve_log)
    }

    /// Take accumulated deltas and reset
    pub fn take_delta(&mut self) -> CreditsDelta {
        let delta = CreditsDelta {
            documents_served: self.delta_documents_served,
            bytes_served: self.delta_bytes_served,
        };
        self.delta_documents_served = 0;
        self.delta_bytes_served = 0;
        delta
    }

    /// Format session uptime
    fn session_uptime(&self) -> String {
        let started = match &self.session_started_at {
            Some(s) => s,
            None => return "0m".to_string(),
        };

        let started_at = chrono::DateTime::parse_from_rfc3339(started)
            .map(|dt| dt.timestamp())
            .unwrap_or(0);

        let elapsed = Utc::now().timestamp() - started_at;
        let hours = elapsed / 3600;
        let mins = (elapsed % 3600) / 60;

        if hours > 0 {
            format!("{}h {}m", hours, mins)
        } else {
            format!("{}m", mins)
        }
    }

    /// Record a work completion event for the activity feed
    pub fn record_work_event(&mut self, work_type: &str, work_id: &str, credits_earned: f64) {
        let now = chrono::Utc::now();

        self.credits_earned += credits_earned as u64;

        let msg = format!("{} completed (+{:.0} cr)", work_type, credits_earned);

        let event = ServeEvent {
            document_id: work_id.to_string(),
            bytes: 0,
            timestamp: now.to_rfc3339(),
            message: msg,
            token_id: String::new(),
            event_type: format!("work_{}", work_type),
        };

        self.recent_events.insert(0, event);
        if self.recent_events.len() > MAX_RECENT_EVENTS {
            self.recent_events.truncate(MAX_RECENT_EVENTS);
        }
    }

    /// Record a sync event for the activity feed
    pub fn record_sync_event(&mut self, direction: &str, document_id: &str, bytes: u64) {
        let now = chrono::Utc::now();

        let event_type = if direction == "push" { "sync_push" } else { "sync_pull" };
        let msg = format!("{} {} ({})", if direction == "push" { "Pushed" } else { "Pulled" }, document_id, format_bytes(bytes));

        let event = ServeEvent {
            document_id: document_id.to_string(),
            bytes,
            timestamp: now.to_rfc3339(),
            message: msg,
            token_id: String::new(),
            event_type: event_type.to_string(),
        };

        self.recent_events.insert(0, event);
        if self.recent_events.len() > MAX_RECENT_EVENTS {
            self.recent_events.truncate(MAX_RECENT_EVENTS);
        }
    }

    /// Get stats for dashboard
    pub fn dashboard_stats(&self) -> DashboardStats {
        // Return most recent 100 events for the UI
        let recent: Vec<ServeEvent> = self.recent_events.iter().take(100).cloned().collect();

        DashboardStats {
            documents_served: self.documents_served,
            pulls_served_total: self.pulls_served_total,
            credits_earned: self.credits_earned,
            total_bytes_served: self.total_bytes_served,
            total_bytes_formatted: format_bytes(self.total_bytes_served),
            today_documents_served: self.today_documents_served,
            today_bytes_served: self.today_bytes_served,
            session_documents_served: self.session_documents_served,
            session_bytes_served: self.session_bytes_served,
            session_uptime: self.session_uptime(),
            total_uptime_seconds: self.total_uptime_seconds,
            first_started_at: self.first_started_at.clone(),
            achievements: compute_achievements(self),
            recent_events: recent,
            server_credit_balance: self.server_credit_balance,
        }
    }
}

/// Delta metrics accumulated between reporting periods
#[derive(Debug, Serialize, Deserialize)]
pub struct CreditsDelta {
    pub documents_served: u64,
    pub bytes_served: u64,
}

impl CreditsDelta {
    pub fn has_data(&self) -> bool {
        self.documents_served > 0 || self.bytes_served > 0
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardStats {
    pub documents_served: u64,
    pub pulls_served_total: u64,
    pub credits_earned: u64,
    pub total_bytes_served: u64,
    pub total_bytes_formatted: String,
    pub today_documents_served: u64,
    pub today_bytes_served: u64,
    pub session_documents_served: u64,
    pub session_bytes_served: u64,
    pub session_uptime: String,
    pub total_uptime_seconds: u64,
    pub first_started_at: Option<String>,
    pub achievements: Vec<Achievement>,
    pub recent_events: Vec<ServeEvent>,
    pub server_credit_balance: f64,
}

/// Report batched serves to the Wire API
pub async fn report_serves(
    api_url: &str,
    access_token: &str,
    node_id: &str,
    serves: &[ServeLogEntry],
) -> Result<(), String> {
    if serves.is_empty() {
        return Ok(());
    }

    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/node/serve-log", api_url);

    let body = serde_json::json!({
        "node_id": node_id,
        "serves": serves,
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Serve log report failed: {}", e))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Serve log report response: {}", text);
    } else {
        tracing::debug!("Reported {} serves to Wire API", serves.len());
    }

    Ok(())
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
