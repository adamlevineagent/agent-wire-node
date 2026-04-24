// walker-v3-completion Wave 6 — dispatch-spine regression fence.
//
// Plan: docs/plans/walker-v3-completion-decision-attachment.md §4.1, §6 Wave 6.
//
// Greps the non-test source surface for patterns that previously bypassed
// walker-v3's DispatchDecision spine. Asserts each is either (a) absent
// entirely OR (b) accompanied by an explicit follow-up to attach Decision
// (via `with_dispatch_decision_if_available`).
//
// Rules enforced:
//
// 1. `make_step_ctx_from_llm_config_with_model` — this legacy W3c
//    workaround was deleted in Wave 6. Zero mentions allowed outside
//    historical comments and this fence test itself.
//
// 2. `make_step_ctx_from_llm_config(` at call sites must NOT be the
//    deleted non-slot variant. Canonical helper requires a slot param;
//    calls with fewer than 8 positional arguments would be the old shape
//    and fail to compile, so this fence is belt-and-suspenders.
//
// 3. Non-test `StepContext::new(` usage for LLM dispatch — every site
//    must be paired with `with_dispatch_decision_if_available` within
//    30 lines OR be in the explicit allowlist below.

use std::fs;
use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.push("src");
    d
}

fn walk_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rust_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn collect_rust_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_rust_files(&src_dir(), &mut out);
    out
}

fn read_file(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

/// Lines of the file, with 1-indexed line numbers. Includes empty lines.
fn numbered_lines(body: &str) -> Vec<(usize, &str)> {
    body.lines().enumerate().map(|(i, s)| (i + 1, s)).collect()
}

/// True if this `StepContext::new` site is inside a `#[cfg(test)]` or
/// `#[test]`/`#[tokio::test]` context (cheap heuristic: the nearest
/// preceding cfg-test line appears before the nearest preceding `mod `
/// or `fn ` that isn't also cfg-gated).
fn is_in_test_context(lines: &[(usize, &str)], line_idx: usize) -> bool {
    // Walk backward looking for either a test marker or a non-test mod/fn.
    for i in (0..line_idx).rev() {
        let (_, content) = lines[i];
        let trimmed = content.trim();
        if trimmed.starts_with("#[cfg(test)]")
            || trimmed.starts_with("#[test]")
            || trimmed.starts_with("#[tokio::test]")
        {
            return true;
        }
        if trimmed.starts_with("mod tests") {
            return true;
        }
    }
    false
}

/// Explicit allowlist for `StepContext::new` sites that legitimately must
/// skip the canonical helper. Format: (relative_path_from_src, reason).
/// Each entry is a file where `StepContext::new` is followed by
/// `with_dispatch_decision_if_available` within 30 lines OR is a
/// documented cache-bypass exception.
const STEP_CONTEXT_NEW_ALLOWLIST: &[(&str, &str)] = &[
    // reroll.rs deliberately sets prompt_hash = "" so cache_is_usable() = false.
    // The canonical helper always computes prompt_hash, which would defeat
    // the intentional cache bypass. reroll.rs instead calls
    // with_dispatch_decision_if_available(ctx).await explicitly after
    // StepContext::new — see plan §6 Wave 3d and reroll.rs:~155.
    (
        "pyramid/reroll.rs",
        "intentional cache bypass; Decision attached explicitly",
    ),
    // step_context.rs is where StepContext::new is defined.
    ("pyramid/step_context.rs", "definition site"),
];

fn allowlisted(relative_path: &str) -> bool {
    STEP_CONTEXT_NEW_ALLOWLIST
        .iter()
        .any(|(p, _)| relative_path.ends_with(p))
}

#[test]
fn no_legacy_with_model_helper_calls() {
    let src = src_dir();
    for path in collect_rust_files() {
        let body = read_file(&path);
        for (lineno, content) in numbered_lines(&body) {
            // Allow mentions in comments (// starts) and in this test file.
            let trimmed = content.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("/*") {
                continue;
            }
            if content.contains("make_step_ctx_from_llm_config_with_model") {
                let rel = path.strip_prefix(&src).unwrap_or(&path).display();
                panic!(
                    "walker-v3-completion regression: {rel}:{lineno} references deleted \
                     helper `make_step_ctx_from_llm_config_with_model`. Migrate to the \
                     canonical `make_step_ctx_from_llm_config` (takes a slot arg).\n\
                     Line: {content}",
                );
            }
        }
    }
}

#[test]
fn step_context_new_outside_test_is_allowlisted_or_decision_aware() {
    let src = src_dir();
    for path in collect_rust_files() {
        let body = read_file(&path);
        let lines = numbered_lines(&body);
        let rel = path
            .strip_prefix(&src)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        if allowlisted(&rel) {
            continue;
        }

        for (i, (lineno, content)) in lines.iter().enumerate() {
            // Skip comment-only lines.
            let trimmed = content.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("/*") {
                continue;
            }
            // Word-boundary match: `StepContext::new(` not preceded by an
            // identifier char (rules out `CacheStepContext::new` and similar).
            let Some(pos) = content.find("StepContext::new(") else {
                continue;
            };
            let prev = content[..pos].chars().last();
            if let Some(ch) = prev {
                if ch.is_alphanumeric() || ch == '_' {
                    continue;
                }
            }
            // Allow if the line is inside a test module or under a
            // cfg(test) / #[test] / #[tokio::test] annotation.
            if is_in_test_context(&lines, i) {
                continue;
            }
            // Look for `with_dispatch_decision_if_available` within 30
            // lines after this site.
            let window_end = (i + 30).min(lines.len());
            let mut ok = false;
            for (_, follow) in &lines[i..window_end] {
                if follow.contains("with_dispatch_decision_if_available") {
                    ok = true;
                    break;
                }
            }
            if !ok {
                panic!(
                    "walker-v3-completion regression: {rel}:{lineno} uses \
                     StepContext::new outside test context and is not followed \
                     by with_dispatch_decision_if_available within 30 lines. \
                     Migrate to make_step_ctx_from_llm_config (takes a slot arg), \
                     OR add an explicit call to with_dispatch_decision_if_available \
                     if cache-bypass is intentional, OR add to STEP_CONTEXT_NEW_ALLOWLIST \
                     with a reason.\nLine: {content}",
                );
            }
        }
    }
}
