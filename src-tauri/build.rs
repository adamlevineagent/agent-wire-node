fn main() {
    // Phase 0.5 skeleton placeholder. WS-D will extend this to compute
    // content hashes for static assets and emit asset_manifest.rs.
    println!("cargo:rerun-if-changed=assets/");
    tauri_build::build()
}
