# Handoff: Folder Nodes as Editable Checklists

**Date:** 2026-04-11
**From:** Session that shipped the Phase 18 Send-error fix, the folder-ingestion-checkbox encoding fix, and the bundled ignore-pattern fix — then hit a 65-build FAILED dispatch and pivoted into design discussion with Adam about the fundamental ingestion model.
**To:** A fresh session (preferably with full context headroom) taking over the folder-ingestion rework.
**Audience:** Claude. Adam is the decision-maker; this doc captures what he's already decided and what he's left open for the next session to propose.

---

## TL;DR

1. The current folder-ingestion model (scan → build all → user watches builds either succeed or fail) is being **replaced** with a scan-then-curate model.
2. The new model: **each folder gets a `.understanding/` directory containing a filemap file that enumerates every file the scanner found, with scanner-suggested inclusion decisions and content types. The user curates the file (in-place in their editor OR via a UI that reads/writes it). The build step reads the curated checklists across the tree and builds only what the user checked.**
3. The canonical spec for this is `docs/vision/self-describing-filesystem.md` (Apr 2026, 409 lines, Adam + Claude session partner). Read it in full before touching code. Today's session proposed an extension that makes the filemap user-editable and filesystem-native rather than SQLite-internal — Adam accepted the extension and made several concrete decisions you must respect (see "Decisions already made" below).
4. There is also a **live production bug** that needs diagnosis independently of the architectural pivot: after shipping today's fixes, Adam ran folder ingestion on `agent-wire-node/` and hit 65 builds all in `FAILED (idle) 0/0 steps $0.000` state. Affects both local mode AND OpenRouter (he verified). Unknown root cause. Adam opted to stop that investigation to do the architectural rethink. This bug is in scope for you.
5. You have been handed an app that:
   - Builds cleanly (`cargo check` + `cargo tauri build` both green)
   - Has a functioning Mac bundle at `src-tauri/target/release/bundle/macos/Wire Node.app` timestamped `Apr 11 08:27:09 2026`
   - Has the folder-ingest checkbox encoding fix, the `.claude/`/`.lab.bak.`/`~/` bundled ignore patterns, and the Phase 18 Send-error fix all present
   - But the actual ingestion dispatch blows up immediately when triggered against a real folder.

---

## Required reading (in order)

1. **`docs/vision/self-describing-filesystem.md`** — the whole spec, especially "Folder Nodes — The Immediate Bridge" (§46-142), "`.understanding/`-per-folder" (§145-235), and "Relationship to the Current 17-Phase Plan" (§323-350).
2. **This document** — captures today's extension, Adam's answers to clarifying questions, and the open threads.
3. **`~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_folder_nodes_checklist.md`** — the memory file capturing the pivot so subsequent sessions keep it (I'm writing it alongside this handoff).
4. **`src-tauri/src/pyramid/folder_ingestion.rs`** — the current Phase 17 folder walker. `scan_folder`, `path_matches_any_ignore`, `extract_build_dispatches`, and the CC mini-subplan construction are the code paths that change most under the new model.
5. **`src-tauri/assets/bundled_contributions.json`** (line 148, the `folder_ingestion_heuristics` contribution) — the canonical home for default patterns; kept in sync with `default_ignore_patterns()` in `src-tauri/src/pyramid/db.rs` line 14003.

---

## The extension Adam accepted today

The spec describes folder nodes with an **auto-computed `filemap` payload** (covered / uncovered / deleted / coverage_ratio / child_folder_ids) stored **inside a SQLite `pyramid_nodes` row**. The computation happens at scan time; the user sees the result only as a number in the preview.

Today's pivot externalizes the filemap:

> Scan writes a filemap file into each folder as part of the folder-node creation. The file is the canonical folder node. SQLite becomes a derived cache of the filemap files, not the source of truth for folder-node state.

The practical consequences:

- **No LLM work happens during "ingest"** — ingesting is free and idempotent; it just walks the tree and writes filemap files.
- **Build is a separate, later step** that reads all filemap files in the target tree, assembles a plan of only the user-checked entries, and dispatches builds. This is what today's "Ingest" button has to become.
- **The user curates between those two steps** — either by editing the filemap files directly in their editor (making this a filesystem-native workflow) or by using a UI that reads and writes them (making it ergonomic for users who don't want to open dozens of yaml files).
- **Re-scanning is a field-level diff**, not a replay: scanner only touches scanner-owned fields (hashes, sizes, mtimes, detected content_type); user-owned fields (included/excluded, content_type override, notes, annotations) are preserved across re-scans.
- **The filemap file also accumulates build history**: "this node was built on date X, pyramid node ID Y, content-hash Z, last error W". Debugging folder-level ingestion becomes "open the filemap file in an editor."

This is the same principle the spec already argues for (`.understanding/` per folder, SQLite as cache, files as canonical) applied specifically to the filemap. It's the minimum-viable migration that proves the bigger spec's thesis on one file type before touching anything else. Once filemap files are canonical, the remaining contents of `.understanding/` (node payloads, evidence, edges, conversations, cache) can migrate incrementally on the same pattern.

## Decisions already made (don't re-ask Adam)

Adam answered four clarifying questions today. These are **decisions**, not suggestions, and the next session must respect them:

1. **Use the full `.understanding/` layout from the start, not a bridging `.filemap.yaml` at folder root.**
   > "I don't really see any reason *not* to do it with the full map in each folder and figure out/normalize to that reality."

   So the filemap file lives **inside** `.understanding/` — probably at `.understanding/folder.md` as the spec originally described, or `.understanding/filemap.yaml` if you prefer yaml semantics, but the containing directory is `.understanding/` from day one, not an ad-hoc root-level hidden file. The rest of `.understanding/` (nodes/, edges/, evidence/, configs/, conversations/, cache/) doesn't need to be populated yet, but the directory IS created and the filemap file goes inside it.

   The exact file name + format inside `.understanding/` is still yours to propose — just propose ONE and get Adam's nod before shipping.

2. **User has full control. Scanner produces a best-guess baseline, user overrides everything they disagree with.**
   > "Paradigm is that we take our best guess at what we think they probably want and what stuff is and then we tell them to make sure its right and correct everything thats not, so they need full control if they want it."

   Concretely this means:
   - The filemap file schema must distinguish **scanner-owned fields** (hashes, sizes, mtimes, detected content_type, inclusion-suggestion) from **user-owned fields** (user_included, user_content_type_override, user_notes, user_annotations).
   - Re-scans rewrite scanner-owned fields and leave user-owned fields untouched.
   - When scanner-detected content_type disagrees with user override, user override wins. Every single time.
   - When a new file appears on disk that isn't in the filemap, it's added with `user_included: null` (tri-state: not yet curated) and scanner-suggested defaults. It does NOT get auto-included or auto-excluded — it waits for the user.
   - When a file in the filemap disappears from disk, it moves to the `deleted:` tombstone list with the date and the pyramid node ID it produced before deletion. The user can still see it; the builder ignores it.

3. **The curation UI and the filesystem file coexist. Neither replaces the other.**
   > "I think it coexists with it because users don't prefer to go into every folder to set up text files, you know? The interface is just an easier way to manage it for the human."

   The filesystem file is canonical (so the paradigm, portability, and tooling integration work). The UI is an ergonomic reader/writer on top of it. Users who prefer their editor can ignore the UI; users who prefer the UI can ignore the files. Both paths must stay consistent — the UI reads the file, displays it, writes changes back, and a subsequent `cat` of the file shows exactly what the user set.

   Design implication: do NOT build a UI that keeps its own state in SQLite and syncs to files. The UI must read the file on open and write the file on every mutation. The filesystem IS the model.

4. **Today's stopgap fix (the checkbox-in-accordion idea I proposed earlier in the session) is subsumed, not pursued in parallel.**

   The per-folder filemap approach replaces the "make the `<details>` operation list interactive with checkboxes" idea. Don't build the stopgap — ship the real thing. If the real thing takes longer than one session, the baseline (what's already in main today) is acceptable interim UX.

5. **Phase placement for this work is OPEN.**
   > "I'll check your context but I think you write the handoff and then answer questions for the new guy, you've been going a long time."

   Adam did not decide whether this lands inside the current 17-phase plan as a post-Phase-17 addendum, or as Phase 1 of the "Self-Describing Filesystem" next initiative (which the spec describes at §354-371). You have two tasks here:

   a) Make a recommendation — the main argument for landing it inside the current initiative is that **Phase 17's output (folder ingestion) is directly blocked by the 65-FAILED-dispatch bug, and the new model would route around it structurally.** The main argument for punting to the next initiative is that this is a major architectural change and the current initiative is supposed to be wrapping up, not expanding. Present both sides; let Adam decide.

   b) If he picks "inside current initiative", propose a phase number + subphases. If he picks "next initiative", propose an outline that respects the 8-phase sketch already in the spec §360-369 but inserts this as the first concrete phase.

## Today's production bug (diagnose this regardless of the pivot)

While shipping the pattern fix, Adam tested the full pipeline and hit an all-red dispatch. Details:

- He triggered folder ingestion on `/Users/adamlevine/AI Project Files/agent-wire-node`
- Sidebar showed **"Understanding: 129 pyramids"** — unexpected, preview had said ~79 (or lower after the pattern fix; we never got a clean re-preview number)
- Builds menu showed **65 active builds**, **all in `FAILED (idle) 0/0 steps $0.000`**
- The slugs in the screenshot (`src-partner`, `icons-ios`, `yaml-renderer-widgets`, `components-yaml-renderer`, `components-stewardship`) look like **GoodNewsEveryone** frontend structure, NOT `agent-wire-node`. Adam may have fired on the wrong directory, OR there may be stacked scans from multiple roots, OR the bug is rendering the wrong slugs.
- Adam tested with both local mode (Ollama) AND OpenRouter. Both failed identically. **This rules out local mode tier-routing as the root cause.** It's upstream of provider resolution.
- Adam did not click "View" on any of the failed builds to see the error detail from `BuildHandle.error`. If you repro, that's the first diagnostic step.

### What "FAILED (idle) 0/0 steps" means in the code

From my earlier investigation (see `src-tauri/src/main.rs` lines 6541-6586 for `pyramid_active_builds`):

- The build got into the `active_build` map (so dispatch reached it)
- The `BuildHandle.status` was set to `"failed"` before any step wrote a row to `pyramid_pipeline_steps` or `pyramid_step_cache`
- `completed_steps` and `total_steps` both query counts of `pyramid_pipeline_steps` for `(slug, build_id)` — zero because no step ever completed
- `current_step` is `None` because `pyramid_active_builds` passes it as `None` unconditionally in this path

For all 65 to fail this way at once, the failure is a shared-state issue that every dispatch hits the same way:

**Hypothesis list (prioritized):**

1. **Chain resolution failure.** The scanner tagged the pyramids with a `content_type` that doesn't have a registered chain, OR the chain registry lookup fails because a dependency (chain binding, skill contribution) isn't wired. Every build tries to look up its chain and fails identically. Check: where does the build_runner resolve the chain for a `content_type: code` (or `document`, or whatever) bedrock build? What happens if that resolution errors?

2. **Plan load failure.** The first thing the build_runner does when kicked off is read the plan (the chain YAML + any pre-build assertions). If the plan file is missing or malformed, every build will fail at that step. Check: is there a plan-loading step that happens before the first pipeline step is recorded? Where?

3. **Provider registry race.** The build starts, tries to acquire a provider from the registry, the registry is in a bad state (e.g., local mode partially wired, OpenRouter key not set in the session's environment), build fails immediately. Adam saying "OpenRouter fails immediately too, although maybe that's switching-back-from-local-only-config-issue" suggests he suspects this. It's worth ruling out by fully restarting the app between local and OpenRouter tests.

4. **DB write contention / transaction abort.** The first `INSERT` into `pyramid_pipeline_steps` at step 1 blocks on a writer lock that the dispatcher still holds, the build times out or rolls back, status becomes failed. Less likely given the Phase 18 Send-error fix we just shipped splits prepare/commit phases, but worth checking in the dispatcher.

5. **Slug mismatch from today's pattern fix.** Unlikely but possible: the `.claude/` exclusion might now be suppressing a folder the CC auto-include feature was counting on finding. Check if the CC conversation discovery runs AFTER the bundled ignore filter or independently of it. The pattern should only affect top-level scanning, but if a code path is walking `.claude/` itself for CC scanning, the new `.claude/` exclusion could starve it.

### How to reproduce locally

1. Launch the current Wire Node bundle (`Apr 11 08:27:09` — already installed OR copy from the build dir to /Applications)
2. Go to Add Workspace, pick `/Users/adamlevine/AI Project Files/agent-wire-node`, trigger the preview, trigger the ingest
3. Watch the Builds tab — expect the 65 FAILED state
4. Click "View" on one to get the actual error message from `BuildHandle.error` — this was the diagnostic step Adam didn't take, and it will probably tell you the root cause in one line

If the bug turns out to be in the dispatch path and is quick to fix, ship the fix before starting the architectural pivot — it's strictly smaller scope and will let Adam keep testing while the bigger work is in flight. If the bug turns out to be deep (e.g., chain binding for the new content types), you may need to coordinate the fix with the pivot.

## Today's other work (context for the pivot, not itself the pivot)

This session also shipped three concrete fixes that are all in main. You inherit them; don't undo them.

### 1. Phase 18 Send-error fix (from earlier in the session)

7 Send trait errors in 3 Tauri command handlers were blocking the Mac build after the Phase 18 merge:
- `pyramid_enable_local_mode`, `pyramid_disable_local_mode`, `pyramid_publish_to_wire` were each holding a `!Send` rusqlite Connection across an `.await`
- Fix was to split prepare/commit phases in `src-tauri/src/pyramid/local_mode.rs` (new `prepare_enable_local_mode` async, new `commit_enable_local_mode` sync, etc.) and to drop a vestigial `async` keyword from `wire_publish::export_cache_manifest` that had zero awaits in its body
- Also removed a bogus `#[serde(default)]` attribute on a Tauri command parameter
- Learning: **`cargo check --lib` does NOT elaborate binary crate command futures, so Send errors on Tauri command handlers only surface under `cargo check` (default target) or `cargo check --bin`.** All future workstream ceremony must use the default target as the gate. This is captured in `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/feedback_cargo_check_lib_insufficient_for_binary.md` and I updated `~/.claude/skills/wire-node-build/SKILL.md` to enforce it.

### 2. Claude Code folder-ingest checkbox fix

The "include claude code conversations" checkbox in the folder ingestion wizard was always greyed out, regardless of the selected path. Two bugs:

- `encode_path_for_claude_code` only replaced `/` with `-`, but Claude Code collapses **all non-alphanumeric-non-dash characters to `-`** — so any path with spaces or dots (e.g., `/Users/adamlevine/AI Project Files/...`) produced the wrong encoded string and the lookup in `~/.claude/projects/` missed. Fix: rewrite the encoder to collapse every non-alphanumeric-non-dash char to `-`.
- The function `find_claude_code_conversation_dirs` assumed "Pattern A" only: an encoded directory inside `~/.claude/projects/` containing JSONL files directly. It didn't handle "Pattern B": the user pointing at a directory that itself contains `.jsonl` files directly (no encoded parent). Fix: added a Pattern B fallback in `find_claude_code_conversation_dirs`, a `directly_contains_jsonls` helper, and updated `describe_claude_code_dirs` to mark Pattern B matches with `is_main=true, is_worktree=false`.
- 4 new regression tests added to `folder_ingestion.rs` under `phase17_tests`, all green.

### 3. Bundled ignore pattern additions (this session)

Adam ran the ingestion preview on `agent-wire-node/` and saw 79 pyramids with 7 ignored — way too many. Root cause: `.lab.bak.*/` (7 timestamped experiment backup dirs), `.claude/worktrees/pedantic-hypatia/` (CC workstream tree duplicate), and a literal `~/Library/Application Support/wire-node/` dir (shell-escape mishap) were all leaking through because `.gitignore` only had `.lab/`, not `.lab.bak.*`, and had no rules for `.claude/` or `~/`.

Added to both `default_ignore_patterns()` in `src-tauri/src/pyramid/db.rs` AND the bundled `folder_ingestion_heuristics` contribution in `src-tauri/assets/bundled_contributions.json`:

- `.claude/` (CC worktrees + handoff docs + session state — CC conversations are still ingested separately via `claude_code_conversation_path`)
- `.lab.bak.` (substring match — catches any `.lab.bak.<timestamp>/` dir without requiring per-timestamp enumeration)
- `~/` (literal tilde dir at repo root)
- Plus defence-in-depth: `.next/`, `.nuxt/`, `.turbo/`, `.pytest_cache/`, `.mypy_cache/`, `.ruff_cache/`, `.idea/`, `.vscode/`, `coverage/`, `.nyc_output/`, `out/`, `.svn/`, `.hg/`

Added 3 regression tests to `folder_ingestion.rs::phase17_tests`:
- `test_path_matches_any_ignore_lab_bak_substring` — validates substring + negative cases
- `test_path_matches_any_ignore_claude_directory` — validates directory component match + negative cases
- `test_path_matches_any_ignore_literal_tilde_directory` — validates literal `~/` + negative cases

All 5 pattern-matcher tests pass. `cargo check` clean.

**Important:** this fix is a baseline improvement but does NOT solve Adam's "a lot of pyramids" concern structurally. That's what the pivot is for. The pattern fix should stay in main because it helps every user on every scan, but it's not the answer.

### 4. Documentation additions

- `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/feedback_cargo_check_lib_insufficient_for_binary.md` — captures the Phase 18 Send-error lesson
- `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_pyramid_include_allowlist.md` — captures an earlier `.pyramidinclude` allow-list idea from the middle of this session (now subsumed by the filemap-file-per-folder decision)
- `~/.claude/skills/wire-node-build/SKILL.md` — updated to require `cargo check` default target as the pre-build gate, with a Failure Modes table entry for Send-across-await

## Open design questions for you to propose

The four decisions above are settled. Everything below is for you to propose and get Adam's nod on. Don't assume a direction; present options and pick your recommendation.

### Q1. File format and schema for the filemap file

Two plausible directions:

- **`.understanding/folder.md`** (matches the spec exactly): markdown with a YAML frontmatter block for scanner-owned fields and a markdown body for user-owned content, including a checklist section using standard markdown checkbox syntax (`- [x] path/to/file.rs`). Users can edit it in any markdown editor.
- **`.understanding/filemap.yaml`**: pure YAML with clear `scanner:` and `user:` top-level keys. Rigid structure; machine-parseable; awkward in a pure editor without yaml syntax highlighting.

Recommendation to write up: hybrid. Markdown frontmatter holds scanner-owned fields (hashes, sizes, mtimes, detected content_type, scan timestamp). Markdown body holds user-editable content (checklist, notes, overrides) with a well-defined section syntax so the writer can parse it unambiguously. This matches the spec's "git-friendly" hint and the "users edit in any editor" ergonomics, while keeping the scanner's job simple.

Either way, the spec's five uncovered categories (`excluded_by_pattern`, `excluded_by_size`, `excluded_by_type`, `unsupported_content_type`, `failed_extraction`) must be in the format.

### Q2. Scanner-owned vs user-owned field boundary

Propose a specific list and get Adam to ratify. Rough starting point:

| Field | Owner | Notes |
|---|---|---|
| `path` | scanner | key |
| `size_bytes` | scanner | |
| `mtime` | scanner | |
| `sha256` | scanner | |
| `detected_content_type` | scanner | scanner's best guess |
| `detected_inclusion` | scanner | scanner's best guess (excluded by pattern/size/type, or included, or unsupported) |
| `user_included` | user | tri-state: `null`, `true`, `false` — `null` means "not yet curated"; both `true` and `false` are explicit user decisions that override scanner |
| `user_content_type` | user | overrides `detected_content_type` when set |
| `user_notes` | user | free text |
| `built_as_pyramid_node` | scanner (post-build) | the pyramid node ID produced for this file in the last build |
| `last_build_at` | scanner (post-build) | when this entry was last built |
| `last_build_error` | scanner (post-build) | error message if last build failed |

The "post-build" rows are a new category — scanner-writable but only after a build, not during scan. Worth calling out explicitly so the post-build writer doesn't stomp user fields.

### Q3. Re-scan conflict policy

Propose: scanner re-scans only touch scanner-owned rows. If a new file appears on disk, add it with `user_included: null`. If a file disappears, move to `deleted:` list with the scanner's record of the last-known state (hash, size, last pyramid node ID). User fields are never touched by the scanner. Field-level merge, not line-level.

Edge case to call out: the user renames a file. The scanner sees "file A deleted, file B new" — it doesn't know they're the same thing. For v1, treat rename as delete + add. For v2, optionally detect renames by matching sha256 between the deleted and new entries.

### Q4. Inheritance / cascading between folder filemaps

A parent folder's filemap may want to say "all my subfolders default to unchecked" or "this entire subtree is excluded". Propose:

- An optional `children_default: include | skip | unchecked` key in the filemap's metadata
- An optional `children_ignore_patterns` key that layers additional ignore patterns on top of the bundled defaults for subfolder scans rooted under this folder
- Inheritance is always-additive: a parent can add exclusions, never remove them
- A subfolder's filemap can override its parent's `children_default` if it wants to

This is where the declarative model starts to earn its keep — once checklist files inherit, users curate at the level of abstraction they care about, not at the level of individual files.

### Q5. Build-from-checklists-tree execution path

Currently, `cargo tauri ... ingest` runs one scan and one dispatch. The new model splits these:

- `wire-node scan <root>`: walks the tree, writes `.understanding/folder.md` (or whatever format) files. No LLM work. Idempotent. Cheap.
- `wire-node build <root>`: walks the tree, reads every `.understanding/folder.md`, assembles a plan of all user-checked entries across the tree, dispatches builds in dependency order (vines before bedrocks in general, CC mini-subplans where applicable). This is where the current `extract_build_dispatches` and the dispatcher go.

Propose the smallest change to the existing dispatcher that lets it accept a plan-assembled-from-N-checklists rather than a single-scan plan. Don't rewrite the dispatcher; adapt its input.

### Q6. Migration from existing pyramids

Users who have already built pyramids under the old model have folder nodes with no `.understanding/` directories. Two migration strategies:

- **Forward-only**: new scans use the new model, existing pyramids stay on the old model until they're re-scanned. Simplest. Probably correct.
- **Retroactive**: walk existing folder nodes, emit a `.understanding/folder.md` for each, mark everything that was built as `user_included: true` and everything that wasn't seen as `user_included: null`. More user-friendly but more code.

Recommend forward-only for v1 unless Adam says otherwise.

### Q7. Dependency on the 65-FAILED bug

If the dispatcher is fundamentally broken today (which the 65-FAILED state strongly suggests), you have a choice:

- **Fix the dispatcher first, then pivot.** Safer because the new model reuses the dispatcher.
- **Pivot first, fix the dispatcher as part of the new build-from-checklists path.** Faster because the new path lets you sidestep the bug.

Diagnose the 65-FAILED first (one click on "View" gives you the error message), then pick. Don't start the pivot without understanding what broke today, because the pivot reuses most of the same code.

## What NOT to do

- **Do not build the "make the `<details>` operation list interactive with checkboxes" stopgap.** That was my earlier suggestion in this session; Adam's pivot supersedes it.
- **Do not touch `folder_ingestion_heuristics` further without reason.** The pattern list is in a good state for what it's supposed to do (produce a sensible default baseline). The real fix is the pivot, not more patterns.
- **Do not migrate to `.understanding/` for node payloads / edges / evidence / cache in this work.** The spec wants all of those there eventually, but today's pivot is scoped to the filemap file ONLY. Adam explicitly said "do it with the full map in each folder" which I read as "use the full `.understanding/` layout for the filemap" — not "migrate everything in the spec at once." If you think I mis-read this, ask Adam.
- **Do not write new ignore patterns for unobserved edge cases.** The current list is what we've observed in Adam's actual filesystem. Don't pre-add stuff nobody has hit yet.
- **Do not skip `cargo check` (default target) on any change that touches Tauri command handlers.** `cargo check --lib` is insufficient for the Send/Sync analysis on binary crate futures. See the memory file referenced above.
- **Do not install the Mac bundle to /Applications without asking.** The current bundle at `src-tauri/target/release/bundle/macos/Wire Node.app` (Apr 11 08:27:09) is the latest; Adam hasn't confirmed whether he wants it in /Applications yet.

## Questions to ask Adam when you arrive

1. **Confirm which question the paper reference resolved to.** (He said "you found it, I call all my docs papers and other terms casually" — so he meant the spec itself is his "paper". But if future references come up, keep verifying.)
2. **Phase placement (Q5 above).** Inside the current 17-phase plan or as the first phase of the next "Self-Describing Filesystem" initiative? Give him your recommendation and let him pick.
3. **Format + schema (Q1-Q2 above).** Propose one hybrid markdown + YAML-frontmatter format, show him a sample file, get his nod.
4. **Migration policy (Q6 above).** Confirm forward-only unless he wants retroactive.
5. **Bug diagnosis (Q7 above).** Share what you found when you clicked "View" on a FAILED build, and confirm the fix-order choice.

## References

- Canonical spec: `agent-wire-node/docs/vision/self-describing-filesystem.md`
- Phase 17 implementation: `src-tauri/src/pyramid/folder_ingestion.rs`
- Current 17-phase plan: `agent-wire-node/docs/plans/pyramid-folders-model-routing-full-pipeline-observability.md`
- Phase 18 handoffs: `handoff-2026-04-09-pyramid-folders-model-routing.md`, `handoff-2026-04-09-pyramid-folders-model-routing-addendum-01.md`
- Phase 18 retro: `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/project_pyramid_folders_17phase_retro.md`
- Send-error lesson: `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/feedback_cargo_check_lib_insufficient_for_binary.md`
- Build skill: `~/.claude/skills/wire-node-build/SKILL.md`
- Memory index: `~/.claude/projects/-Users-adamlevine-AI-Project-Files/memory/MEMORY.md`
