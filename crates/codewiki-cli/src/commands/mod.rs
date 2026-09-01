// T-442 — Commands module.
pub mod serve;
// T-509 — snapshot / restore (non-gating nice-to-have per PLAN.md Revision Log #14)
pub mod snapshot;
// T-430 — real implementations for previously-stubbed subcommands.
pub mod affected;
pub mod callees;
pub mod callers;
pub mod context;
pub mod files;
pub mod impact;
pub mod index;
pub mod init;
pub mod query;
pub mod render;
pub mod status;
pub mod sync;
pub mod uninit;
pub mod util;

// D5 — new onboarding + diagnostics commands.
pub mod doctor;
pub mod setup;

// Self-update: `codewiki upgrade [--check]`.
pub mod upgrade;

// Graph web UI (feature-gated `web` for HTTP deps; stub always present)
pub mod graph;
