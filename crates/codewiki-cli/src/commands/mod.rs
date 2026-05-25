// T-442 — Commands module.
pub mod serve;
// T-509 — snapshot / restore (non-gating nice-to-have per PLAN.md Revision Log #14)
pub mod snapshot;
// T-430 — real implementations for previously-stubbed subcommands.
pub mod util;
pub mod init;
pub mod index;
pub mod sync;
pub mod query;
pub mod context;
pub mod callers;
pub mod callees;
pub mod impact;
pub mod affected;
pub mod files;
pub mod status;
pub mod uninit;

// D5 — new onboarding + diagnostics commands.
pub mod setup;
pub mod doctor;

// Graph web UI (feature-gated `web` for HTTP deps; stub always present)
pub mod graph;
