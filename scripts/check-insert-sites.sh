#!/usr/bin/env bash
# check-insert-sites.sh — Phase 0a-1 commit 4 CI guard.
#
# Asserts that `pyramid_config_contributions` has exactly ONE raw
# `INSERT INTO` statement in non-test production code — the body of
# `write_contribution_envelope` in
# `src-tauri/src/pyramid/config_contributions.rs`. Every other INSERT
# site was refactored to route through that shim.
#
# ┌─ Why the table name must stay fully qualified ───────────────────┐
# │ Production table: `pyramid_config_contributions` (with pyramid_   │
# │ prefix). There is NOT a separate `config_contributions` table,    │
# │ but other repos in this monorepo / in Wire server code do contain │
# │ a string literal `config_contributions` in ~44 unrelated sites    │
# │ (type names, variable names, doc references). The check must      │
# │ therefore grep for the FULL table name including the prefix —     │
# │ dropping `pyramid_` matches the wrong thing.                      │
# └──────────────────────────────────────────────────────────────────┘
#
# Allow-list: the single production INSERT is the shim body. Test
# modules (`#[cfg(test)]` blocks and `tests/` dir) are exempt — they
# seed fixtures directly and do not need to flow through the writer.
#
# Exit 0: exactly one non-test INSERT exists (the shim body).
# Exit 1: zero or more-than-one — indicates either the shim was
#         deleted, or a new call site was added without routing
#         through the writer.
#
# Commit 5 will extend this script to also assert the presence of
# `BEGIN IMMEDIATE TRANSACTION` inside the writer body and the
# `uq_config_contrib_active` unique index in the schema migration.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Collect every raw INSERT site in src-tauri/src/. Split by test vs.
# production at the line level using cfg(test) markers is expensive
# and fragile — instead we reuse the fact that every test module in
# this crate is gated by `#[cfg(test)]` and those markers sit on
# specific known line numbers. Simpler and more robust: take the set
# of INSERT sites and for each, check whether it lives before or
# after the first `#[cfg(test)]` marker in its file.

# Pattern matches `INSERT INTO`, `INSERT OR IGNORE INTO`, and
# `INSERT OR REPLACE INTO` against the production table. Dropping
# any of the OR-variants would allow a new call site to sneak past
# the guard (phase 0a-1 commit 4 initial revision dropped OR IGNORE
# and missed `insert_bundled_contribution` as a result).
hits=$(grep -rnE "INSERT( OR (IGNORE|REPLACE))? INTO pyramid_config_contributions" "$ROOT/src-tauri/src/" || true)

production_hits=0
production_files=()

while IFS= read -r line; do
    [ -z "$line" ] && continue
    file="${line%%:*}"
    lineno="${line#*:}"
    lineno="${lineno%%:*}"

    # Find the first `#[cfg(test)]` line in the file, if any.
    first_cfg_test=$(grep -n "^#\[cfg(test)\]" "$file" | head -1 | cut -d: -f1 || true)

    if [ -z "$first_cfg_test" ]; then
        # No test module — every INSERT in the file counts as production.
        production_hits=$((production_hits + 1))
        production_files+=("$file:$lineno")
    elif [ "$lineno" -lt "$first_cfg_test" ]; then
        production_hits=$((production_hits + 1))
        production_files+=("$file:$lineno")
    fi
    # Otherwise it's inside a #[cfg(test)] module — ignore.
done <<< "$hits"

if [ "$production_hits" -ne 1 ]; then
    echo "FAIL: expected exactly 1 production INSERT into pyramid_config_contributions (the write_contribution_envelope shim body), found $production_hits:" >&2
    for site in "${production_files[@]}"; do
        echo "  $site" >&2
    done
    exit 1
fi

# Sanity check: the single production hit must be inside config_contributions.rs.
single_site="${production_files[0]}"
if [[ "$single_site" != *"config_contributions.rs"* ]]; then
    echo "FAIL: the single production INSERT is not in config_contributions.rs: $single_site" >&2
    exit 1
fi

echo "OK: 1 production INSERT into pyramid_config_contributions (the write_contribution_envelope shim body)."
exit 0
