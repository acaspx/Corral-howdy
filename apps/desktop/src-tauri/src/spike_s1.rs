//! Spike S1 driver (issue #3): shows the corral from Rust and exercises the round trip over the
//! C ABI. Spike code: it stays on the spike branch and is not merged (the finding goes in
//! docs/adr/001-stack.md). Every line it prints starts with `corral-bridge:` so the run script can
//! check the result.

use crate::{bridge, native};
use std::time::Duration;
use tauri::AppHandle;

/// Starts the corral with three fake agents, then runs the probe script on a background thread.
/// Called from Tauri's `setup`, which runs on the main thread.
pub fn start(app: &AppHandle) -> Result<(), native::NativeError> {
    native::start(|result| match result {
        Ok(intent) => println!("corral-bridge: intent {intent:?}"),
        Err(e) => println!("corral-bridge: refused intent: {e}"),
    })?;
    native::set_roster(&bridge::demo_roster(3))?;
    println!("corral-bridge: started; roster of 3 sent");
    let app = app.clone();
    std::thread::spawn(move || script(&app));
    Ok(())
}

/// Runs `f` on the main thread, the only thread CorralNative accepts, and logs the outcome.
fn on_main(
    app: &AppHandle,
    label: &'static str,
    f: impl FnOnce() -> Result<(), native::NativeError> + Send + 'static,
) {
    let posted = app.run_on_main_thread(move || match f() {
        Ok(()) => println!("corral-bridge: {label}: ok"),
        Err(e) => println!("corral-bridge: {label}: {e}"),
    });
    if let Err(e) = posted {
        println!("corral-bridge: {label}: couldn't reach the main thread: {e}");
    }
}

fn script(app: &AppHandle) {
    std::thread::sleep(Duration::from_secs(3));
    on_main(app, "roster of 5 (live update)", || {
        native::set_roster(&bridge::demo_roster(5))
    });
    std::thread::sleep(Duration::from_secs(2));
    on_main(app, "roster of 10 (8 shown, 2 hidden)", || {
        native::set_roster(&bridge::demo_roster(10))
    });
    // Bad input is refused, never trapped on (TP-BRIDGE-03). Expect "rejected".
    on_main(app, "malformed roster", || {
        native::set_roster_bytes(b"{\"v\":1,")
    });
    // Off the main thread Swift refuses and does nothing. Expect "must be called on the main thread".
    match native::set_roster(&bridge::demo_roster(1)) {
        Ok(()) => println!("corral-bridge: off-main roster: ok (unexpected)"),
        Err(e) => println!("corral-bridge: off-main roster: {e}"),
    }
    std::thread::sleep(Duration::from_secs(1));
    // The round trip without a click: Swift emits openAgent through the tap path.
    on_main(app, "probe openAgent(scout)", || {
        native::probe_open_agent("scout")
    });
    println!("corral-bridge: probes done; click a drop to send openAgent from a real tap");
}
