//! The main window's Corral settings commands (M4 item 2, TP-USAGE-15). The corral attaches a
//! handle once it's running; the commands go through it to the corral's order (`corral_order.rs`,
//! where the rules are tested). Before that, they say the corral isn't running.

use corral_desktop_lib::corral_order::{CorralAgentChoice, CorralSettingsView};
use corral_desktop_lib::corral_settings::{CorralControl, CorralSettings};
use std::sync::{Arc, Mutex};

#[test]
fn tp_usage_15_before_the_corral_runs_the_settings_say_so() {
    let settings = CorralSettings::default();
    for err in [
        settings.settings().expect_err("not running"),
        settings
            .choose_shown(vec!["grok".into()])
            .expect_err("not running"),
        settings.keep_order_fixed(true).expect_err("not running"),
    ] {
        assert!(err.contains("isn't running yet"), "{err}");
    }
}

#[derive(Default)]
struct Fake(Mutex<Vec<String>>);

fn view() -> CorralSettingsView {
    CorralSettingsView {
        agents: vec![CorralAgentChoice {
            id: "grok".into(),
            name: "Grok".into(),
            shown: true,
        }],
        keep_fixed: false,
        needs_choice: false,
        max_shown: 8,
    }
}

impl CorralControl for Fake {
    fn settings(&self) -> Result<CorralSettingsView, String> {
        self.0.lock().expect("calls").push("settings".into());
        Ok(view())
    }

    fn choose_shown(&self, ids: Vec<String>) -> Result<CorralSettingsView, String> {
        self.0
            .lock()
            .expect("calls")
            .push(format!("choose {}", ids.join(",")));
        Ok(view())
    }

    fn keep_order_fixed(&self, keep: bool) -> Result<CorralSettingsView, String> {
        self.0.lock().expect("calls").push(format!("keep {keep}"));
        Ok(view())
    }
}

#[test]
fn tp_usage_15_once_attached_the_commands_reach_the_corral() {
    let settings = CorralSettings::default();
    let fake = Arc::new(Fake::default());
    settings.attach(fake.clone());
    assert_eq!(settings.settings().expect("view"), view());
    settings
        .choose_shown(vec!["grok".into(), "claude".into()])
        .expect("chosen");
    settings.keep_order_fixed(true).expect("switched");
    assert_eq!(
        *fake.0.lock().expect("calls"),
        ["settings", "choose grok,claude", "keep true"]
    );
}
