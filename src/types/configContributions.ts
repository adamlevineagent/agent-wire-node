// src/types/configContributions.ts — Phase 10: TypeScript mirrors of the
// Rust serde types Phase 4, 5, 9 ship for the config contribution surface.
//
// These match the structs in:
//   - src-tauri/src/pyramid/config_contributions.rs (ConfigContribution)
//   - src-tauri/src/pyramid/generative_config.rs    (GenerateConfigResponse,
//     RefineConfigResponse, AcceptConfigResponse, ActiveConfigResponse,
//     SyncResult)
//   - src-tauri/src/pyramid/schema_registry.rs      (ConfigSchemaSummary)
//   - src-tauri/src/pyramid/wire_publish.rs         (DryRunReport,
//     CostBreakdown, SupersessionLink, SectionPreview)
//   - src-tauri/src/pyramid/wire_native_metadata.rs (ResolvedDerivedFromEntry)
//   - src-tauri/src/main.rs                         (PublishToWireResponse,
//     CreateConfigContributionResponse, RejectProposalResponse)
//
// Tauri's default serde casing is snake_case (no `#[serde(rename_all)]`),
// so these interfaces use snake_case field names.

/** One row in `pyramid_config_contributions`. Serialized straight from
 *  `config_contributions::ConfigContribution`. */
export interface ConfigContribution {
    id: number;
    contribution_id: string;
    slug: string | null;
    schema_type: string;
    yaml_content: string;
    wire_native_metadata_json: string;
    wire_publication_state_json: string;
    supersedes_id: string | null;
    superseded_by_id: string | null;
    triggering_note: string | null;
    /** One of "active", "proposed", "rejected", "superseded", "draft". */
    status: string;
    /** One of "local", "wire", "agent", "bundled", "migration". */
    source: string;
    wire_contribution_id: string | null;
    created_by: string | null;
    created_at: string;
    accepted_at: string | null;
}

/** Phase 9 `ConfigSchemaSummary` — one entry per registered schema type. */
export interface ConfigSchemaSummary {
    schema_type: string;
    display_name: string;
    description: string;
    has_generation_skill: boolean;
    has_annotation: boolean;
    has_default_seed: boolean;
}

/** Phase 9 `ActiveConfigResponse` — shape returned by `pyramid_active_config`. */
export interface ActiveConfigResponse {
    contribution_id: string;
    yaml_content: string;
    version_chain_length: number;
    created_at: string;
    triggering_note: string | null;
}

/** Phase 9 `GenerateConfigResponse` — shape returned by `pyramid_generate_config`. */
export interface GenerateConfigResponse {
    contribution_id: string;
    yaml_content: string;
    schema_type: string;
    version: number;
}

/** Phase 9 `RefineConfigResponse` — shape returned by `pyramid_refine_config`. */
export interface RefineConfigResponse {
    new_contribution_id: string;
    yaml_content: string;
    schema_type: string;
    version: number;
}

/** Phase 9 `SyncResult` — operational sync outcome reported by accept. */
export interface SyncResult {
    operational_table: string;
    reload_triggered: string[];
}

/** Phase 9 `AcceptConfigResponse` — shape returned by `pyramid_accept_config`. */
export interface AcceptConfigResponse {
    contribution_id: string;
    yaml_content: string;
    version: number;
    triggering_note: string;
    status: string;
    wire_native_metadata: unknown;
    sync_result: SyncResult;
}

/** Phase 5 `CostBreakdown` — embedded in a `DryRunReport`. */
export interface CostBreakdown {
    deposit_credits: number;
    publish_fee: number;
    author_price: number;
    estimated_total: number;
}

/** Phase 5 `SupersessionLink` — one entry in the supersession chain. */
export interface SupersessionLink {
    handle_path: string;
    wire_contribution_id: string | null;
    maturity: string;
    published_at: string | null;
}

/** Phase 5 `SectionPreview` — one entry per `sections` decomposition. */
export interface SectionPreview {
    heading: string;
    contribution_type: string;
    will_publish: boolean;
}

/** Phase 5 `ResolvedDerivedFromEntry` — one entry in the derived_from preview. */
export interface ResolvedDerivedFromEntry {
    kind: string;
    reference: string;
    weight: number;
    allocated_slots: number;
}

/** Phase 5 `DryRunReport` — returned by `pyramid_dry_run_publish`. */
export interface DryRunReport {
    wire_type: string;
    tags: string[];
    visibility: string;
    canonical_yaml: string;
    cost_breakdown: CostBreakdown;
    resolved_derived_from: ResolvedDerivedFromEntry[];
    supersession_chain: SupersessionLink[];
    warnings: string[];
    section_previews: SectionPreview[];
}

/** Phase 5 `PublishToWireResponse` — returned by `pyramid_publish_to_wire`. */
export interface PublishToWireResponse {
    wire_contribution_id: string;
    handle_path: string | null;
    wire_type: string;
    sections_published: string[];
    /** Phase 18c (L4): set to the number of cache entries attached when
     *  the user opted in to `include_cache_manifest`; `null` means the
     *  user did not opt in (default-OFF privacy gate). The frontend
     *  uses this to render "cache manifest included (N entries)" in
     *  the success state. */
    cache_manifest_entries: number | null;
}

/** Phase 4 `CreateConfigContributionResponse` — returned by accept_proposal. */
export interface CreateConfigContributionResponse {
    contribution_id: string;
}

/** Phase 4 `RejectProposalResponse` — returned by reject_proposal. */
export interface RejectProposalResponse {
    ok: boolean;
}

// ─── Phase 14: Wire Discovery + Ranking + Update Polling ────────────────────

/** Phase 14 `DiscoveryResult` — one ranked search result from
 *  `pyramid_wire_discover` / `pyramid_search_wire_configs`. */
export interface DiscoveryResult {
    wire_contribution_id: string;
    title: string;
    description: string;
    tags: string[];
    author_handle: string | null;
    rating: number | null;
    adoption_count: number;
    open_rebuttals: number;
    chain_length: number;
    freshness_days: number;
    /** Computed composite score in [0, 1]. */
    score: number;
    rationale: string | null;
    schema_type: string | null;
}

/** Phase 14 `Recommendation` — one similarity-ranked recommendation
 *  from `pyramid_wire_recommendations`. */
export interface Recommendation {
    wire_contribution_id: string;
    title: string;
    description: string;
    rationale: string;
    score: number;
}

/** Phase 14 `WireUpdateEntry` — one pending Wire supersession update
 *  returned by `pyramid_wire_update_available`. */
export interface WireUpdateEntry {
    local_contribution_id: string;
    schema_type: string;
    slug: string | null;
    latest_wire_contribution_id: string;
    chain_length_delta: number;
    changes_summary: string | null;
    author_handles: string[];
    checked_at: string;
}

/** Phase 14 `PullLatestResponse` — returned by `pyramid_wire_pull_latest`
 *  and `pyramid_pull_wire_config`. */
export interface PullLatestResponse {
    new_local_contribution_id: string;
    activated: boolean;
}

/** Phase 14 `AutoUpdateSettingEntry` — one row returned by
 *  `pyramid_wire_auto_update_status`. */
export interface AutoUpdateSettingEntry {
    schema_type: string;
    enabled: boolean;
}
