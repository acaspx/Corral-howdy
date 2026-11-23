//! Domain types, run lifecycle state machine, supervisor, budgets and OpenTelemetry mapping.
//!
//! Spec: `docs/01-system-design.md §3, §6, §7, §8`. Built milestone by milestone
//! (`docs/03-build-plan.md`), each piece starting from the failing acceptance tests named in its
//! issue (`AGENTS.md`).

pub mod agent_text;
pub mod budget;
pub mod checkpoint_note;
pub mod clock;
pub mod controls;
pub mod corral_view;
pub mod domain;
pub mod event;
pub mod ids;
pub mod lifecycle;
pub mod mention;
pub mod plan_view;
pub mod prompt_bar;
pub mod run_card;
pub mod supervisor;
pub mod trace_tree;
pub mod trace_view;
pub mod usage_rank;
