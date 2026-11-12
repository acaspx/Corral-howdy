//! TP-IPC-05: the main window's capability grants only what the current UI uses (security model T10).
//!
//! The list below is the whole grant. The main window may read, save and remove provider keys
//! (SK-3); the tray and menus are built in Rust, which capabilities don't gate. Adding a
//! permission means adding it here and in `capabilities/default.json` in the same PR, with the
//! feature that needs it; both are reviewed by the code owner. Never add a bundle such as
//! `core:default`.
const MAIN_WINDOW_PERMISSIONS: &[&str] = &[
    "allow-provider-key-status",
    "allow-save-provider-key",
    "allow-remove-provider-key",
];

fn main_window_capability() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/capabilities/default.json");
    let text = std::fs::read_to_string(path).expect("read capabilities/default.json");
    serde_json::from_str(&text).expect("capabilities/default.json is valid JSON")
}

#[test]
fn tp_ipc_05_main_window_capability_lists_only_what_the_ui_uses() {
    let cap = main_window_capability();
    assert_eq!(
        cap["windows"],
        serde_json::json!(["main"]),
        "this capability is for the main window only"
    );

    let granted: Vec<String> = cap["permissions"]
        .as_array()
        .expect("permissions is a list")
        .iter()
        // A permission is a string, or an object with an identifier when it carries a scope.
        .map(|p| {
            p.as_str()
                .or_else(|| p["identifier"].as_str())
                .unwrap_or("<unnamed>")
                .to_string()
        })
        .collect();

    for p in &granted {
        assert!(
            !p.ends_with(":default") && !p.contains('*'),
            "`{p}` is a bundle or wildcard; grant the single permissions a feature needs"
        );
    }
    assert_eq!(
        granted, MAIN_WINDOW_PERMISSIONS,
        "the main window's grant changed"
    );
}
