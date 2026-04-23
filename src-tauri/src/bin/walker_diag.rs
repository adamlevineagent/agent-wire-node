//! Diagnostic: dump what walker_resolver actually sees against an arbitrary DB.
//! Usage: walker_diag <path-to-pyramid.db>
use rusqlite::Connection;
use wire_node_lib::pyramid::walker_resolver::{
    build_scope_cache_pair, resolve_model_list, ProviderType,
};

fn main() {
    let path = std::env::args().nth(1).expect("usage: walker_diag <db-path>");
    let conn = Connection::open(&path).expect("open db");

    println!("=== walker_diag against {path}");
    let pair = match build_scope_cache_pair(&conn) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("BUILD FAILED: {e:?}");
            std::process::exit(1);
        }
    };
    let chain = &pair.chain;
    println!("call_order: {:?}", chain.call_order);
    println!("provider scope keys: {:?}", chain.provider.keys().collect::<Vec<_>>());
    for pt in [
        ProviderType::OpenRouter,
        ProviderType::Local,
        ProviderType::Fleet,
        ProviderType::Market,
    ] {
        let entry = chain.provider.get(&pt);
        println!("---");
        println!("provider {pt:?}: scope_entry present={}", entry.is_some());
        if let Some(e) = entry {
            println!("  contribution_id: {:?}", e.contribution_id);
            println!("  override keys: {:?}", e.overrides.keys().collect::<Vec<_>>());
            if let Some(ml) = e.overrides.get("model_list") {
                println!("  raw model_list value: {ml:?}");
            }
        }
        for slot in ["mid", "extractor", "synth_heavy", "web"] {
            let ml = resolve_model_list(chain, slot, pt);
            println!("  resolve_model_list({slot}, {pt:?}) = {ml:?}");
        }
    }
}

#[allow(dead_code)]
fn _dummy_breaker_inspect() {
    // walker_breaker uses an OnceLock<Mutex<HashMap>> — purely
    // in-memory, lost when the binary exits. To inspect breaker
    // state, the binary must still be running with peek_state
    // exposed via HTTP. Leaving as a doc note for now.
}
