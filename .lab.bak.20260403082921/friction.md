# Friction Log

## 2026-04-02 20:20 PDT — Build mutations moved to IPC, no obvious shell harness

Problem:
The read side of the pyramid system is available over HTTP/CLI, but create/build mutations now return `{"error":"moved to IPC","command":"..."}`. This blocks normal researcher-style rebuild loops from the shell.

Impact:
Prompt/YAML iteration is slower because the experiment loop depends on a desktop UI path instead of a straightforward local command or scriptable API.

Evidence:
- `POST /pyramid/slugs` returns `moved to IPC`
- `POST /pyramid/<slug>/build` returns `moved to IPC`
- Source confirms `pyramid_create_slug` and `pyramid_build` are Tauri commands in `src-tauri/src/main.rs`

Why it matters:
The chain system is now intentionally YAML-driven, but the test loop still has a UI-only bottleneck. That makes autonomous prompt optimization harder than it should be.

Potential fixes:
- Add a local CLI command for `pyramid_create_slug` / `pyramid_build`
- Add a dev-only shell harness that invokes the same Rust build path directly
- Expose a guarded localhost mutation path for local-only research/dev use

## 2026-04-02 20:41 PDT — Mutation sequence is order-sensitive

Problem:
The new CLI mutation path works, but `create-slug`, `ingest`, and `build` must be run serially. Running them in parallel causes a false-negative build failure before chunks are written.

Impact:
Easy to misclassify as a pipeline failure when it is really an invocation-order issue.

Evidence:
- `wire-node.log`: `Build failed for 'vibesmithy-exp1': No chunks found for slug 'vibesmithy-exp1'`
- The ingest completion log for the same slug appears immediately after the failure.

Why it matters:
Research runs need deterministic orchestration or they generate noise.

Potential fixes:
- Document required sequencing in the CLI help text
- Add a combined `build-from-source` helper command
- Make `build` reject early with a friendlier message if no chunks exist yet
