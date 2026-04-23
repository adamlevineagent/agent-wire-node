// pyramid/viz_config.rs — Pyramid Visualization Config
//
// Get/set the `pyramid_viz_config` contribution. Follows the exact
// pattern from `local_mode.rs` experimental_territory (Ollama Phase 6).
//
// No operational table — the viz engine reads the active contribution
// directly. The dispatcher branch in config_contributions.rs logs a
// debug line and emits the standard ConfigSynced event.

use rusqlite::Connection;
use std::sync::Arc;
use tracing::debug;

use crate::pyramid::config_contributions::{
    create_config_contribution, load_active_config_contribution, supersede_config_contribution,
    sync_config_to_operational,
};
use crate::pyramid::event_bus::BuildEventBus;

/// Read the current pyramid viz config contribution.
/// Tries slug-scoped first, then global, then returns a default.
pub fn get_pyramid_viz_config(
    conn: &Connection,
    slug: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    // Try slug-scoped first, then global
    if let Some(s) = slug {
        if let Some(contrib) = load_active_config_contribution(conn, "pyramid_viz_config", Some(s))?
        {
            let val: serde_json::Value = serde_yaml::from_str(&contrib.yaml_content)?;
            debug!(
                slug = s,
                "pyramid_viz_config: loaded slug-scoped contribution"
            );
            return Ok(val);
        }
    }
    // Fall back to global
    match load_active_config_contribution(conn, "pyramid_viz_config", None)? {
        Some(contrib) => {
            let val: serde_json::Value = serde_yaml::from_str(&contrib.yaml_content)?;
            debug!("pyramid_viz_config: loaded global contribution");
            Ok(val)
        }
        None => {
            debug!("pyramid_viz_config: no contribution found, returning default");
            Ok(default_pyramid_viz_config())
        }
    }
}

fn default_pyramid_viz_config() -> serde_json::Value {
    serde_json::json!({
        "schema_type": "pyramid_viz_config",
        "rendering": {
            "tier": "auto",
            "max_dots_per_layer": 10,
            "always_collapse": false,
            "force_all_nodes": false
        },
        "overlays": {
            "structure": true,
            "web_edges": true,
            "staleness": true,
            "provenance": true,
            "weight_intensity": true
        },
        "chronicle": {
            "show_mechanical_ops": false,
            "auto_expand_decisions": true
        },
        "ticker": {
            "enabled": true,
            "position": "bottom"
        },
        "window": {
            "auto_pop_on_build": true
        },
        "density": {
            "repulsion": "auto",
            "attraction": "auto",
            "damping": "auto",
            "settle_threshold": "auto",
            "label_min_radius": "auto",
            "max_iterations": "auto",
            "center_gravity": "auto",
            "max_nodes": "auto"
        }
    })
}

/// Set the pyramid viz config. Creates or supersedes the contribution.
pub fn set_pyramid_viz_config(
    conn: &mut Connection,
    bus: &Arc<BuildEventBus>,
    slug: Option<&str>,
    config_json: serde_json::Value,
) -> anyhow::Result<()> {
    let yaml_str = serde_yaml::to_string(&config_json)?;
    let prior = load_active_config_contribution(conn, "pyramid_viz_config", slug)?;
    if let Some(prior_contrib) = prior {
        supersede_config_contribution(
            conn,
            &prior_contrib.contribution_id,
            &yaml_str,
            "pyramid viz config updated",
            "user",
            Some("user"),
        )?;
    } else {
        create_config_contribution(
            conn,
            "pyramid_viz_config",
            slug,
            &yaml_str,
            Some("pyramid viz config created"),
            "user",
            Some("user"),
            "active",
        )?;
    }
    // Sync (no-op for this type, but fires ConfigSynced event).
    if let Some(contrib) = load_active_config_contribution(conn, "pyramid_viz_config", slug)? {
        sync_config_to_operational(conn, bus, &contrib)?;
    }
    Ok(())
}
