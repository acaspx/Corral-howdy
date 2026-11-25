//! The TypeScript declarations of every type the main window sees (M4 item 1, ADR-001,
//! TP-IPC-06), generated from the Rust types with ts-rs and committed as
//! `apps/desktop/src/bindings.ts`. A test fails when that file is out of date, so the window and
//! the core can't drift. The `invoke` wrappers stay small and hand-written next to the components
//! (`keys.ts`, `providerToolsApi.ts`), typed with these.

use crate::agent_editor::{AgentEdit, AgentEditorView, ModelView};
use crate::agent_policies::{
    AgentPolicyEdit, AgentPolicyView, PolicyClassView, PolicyRuleChange, RuleChoice, RuleOrigin,
    TimeoutChoice,
};
use crate::corral_order::{CorralAgentChoice, CorralSettingsView};
use crate::keys::Connected;
use crate::provider_tools::{ProviderToolChoice, ProviderToolsView};
use crate::traces::{RunListItem, RunListOutcome, RunTrace};
use corral_adapters::agent::ProviderTool;
use corral_core::trace_view::{Attribute, Span, SpanKind, SpanStatus, TraceView};
use corral_secrets::{KeyStatus, Provider};
use ts_rs::{Config, TS};

/// What `apps/desktop/src/bindings.ts` should hold: each type, exported, in a fixed order.
pub fn typescript() -> String {
    let cfg = Config::default();
    let decls = [
        Provider::decl(&cfg),
        KeyStatus::decl(&cfg),
        Connected::decl(&cfg),
        ProviderTool::decl(&cfg),
        ProviderToolChoice::decl(&cfg),
        ProviderToolsView::decl(&cfg),
        RunListItem::decl(&cfg),
        RunListOutcome::decl(&cfg),
        RunTrace::decl(&cfg),
        CorralSettingsView::decl(&cfg),
        CorralAgentChoice::decl(&cfg),
        TraceView::decl(&cfg),
        Span::decl(&cfg),
        Attribute::decl(&cfg),
        SpanKind::decl(&cfg),
        SpanStatus::decl(&cfg),
        AgentPolicyView::decl(&cfg),
        PolicyClassView::decl(&cfg),
        AgentPolicyEdit::decl(&cfg),
        PolicyRuleChange::decl(&cfg),
        RuleChoice::decl(&cfg),
        RuleOrigin::decl(&cfg),
        TimeoutChoice::decl(&cfg),
        AgentEditorView::decl(&cfg),
        ModelView::decl(&cfg),
        AgentEdit::decl(&cfg),
    ];
    let mut out = String::from(
        "// Generated from the Rust types by apps/desktop/src-tauri/src/bindings.rs (TP-IPC-06).\n\
         // Don't edit: change the Rust type, then run\n\
         //   CORRAL_WRITE_BINDINGS=1 cargo test -p corral-desktop --test bindings\n",
    );
    for decl in decls {
        out.push('\n');
        // ts-rs leaves spaces at the ends of some lines.
        let decl: Vec<&str> = decl.lines().map(str::trim_end).collect();
        out.push_str("export ");
        out.push_str(&decl.join("\n"));
        out.push('\n');
    }
    out
}
