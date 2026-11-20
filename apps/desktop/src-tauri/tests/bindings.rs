//! The main window's types are generated from Rust (M4 item 1, ADR-001, TP-IPC-06). Every type
//! that crosses into the window derives `ts_rs::TS`; `bindings::typescript()` writes them all to
//! `apps/desktop/src/bindings.ts`, and this fails when the committed file is out of date, so the
//! two sides can't drift. Regenerate with:
//!
//!     CORRAL_WRITE_BINDINGS=1 cargo test -p corral-desktop --test bindings

use corral_desktop_lib::bindings::typescript;

const COMMITTED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bindings.ts");

#[test]
fn tp_ipc_06_the_committed_bindings_match_the_rust_types() {
    let generated = typescript();
    if std::env::var_os("CORRAL_WRITE_BINDINGS").is_some() {
        std::fs::write(COMMITTED, &generated).expect("write apps/desktop/src/bindings.ts");
    }
    let committed = std::fs::read_to_string(COMMITTED).expect("read apps/desktop/src/bindings.ts");
    assert!(
        committed == generated,
        "apps/desktop/src/bindings.ts is out of date with the Rust types. Regenerate it:\n\n    \
         CORRAL_WRITE_BINDINGS=1 cargo test -p corral-desktop --test bindings\n"
    );
}

#[test]
fn tp_ipc_06_every_type_the_window_sees_is_exported() {
    let ts = typescript();
    for name in [
        "Provider",
        "KeyStatus",
        "Connected",
        "ProviderTool",
        "ProviderToolChoice",
        "ProviderToolsView",
    ] {
        assert_eq!(
            ts.matches(&format!("export type {name} = ")).count(),
            1,
            "{name} is exported once"
        );
    }
    assert!(
        ts.starts_with("// Generated from the Rust types"),
        "says it's generated"
    );
}

#[test]
fn tp_ipc_06_the_types_match_what_serde_sends() {
    let ts = typescript();
    // JSON numbers, not bigints, even for u64 fields such as costs.
    assert!(!ts.contains("bigint"), "{ts}");
    assert!(ts.contains("export type Provider = \"xai\" | \"openai\" | \"anthropic\" | \"meta\";"));
    assert!(ts.contains("export type ProviderTool = \"web_search\";"));
    assert!(
        ts.contains("costMicroUsd: number"),
        "camelCase, as serde renames it"
    );
    assert!(ts.contains("last4: string | null"));
}
