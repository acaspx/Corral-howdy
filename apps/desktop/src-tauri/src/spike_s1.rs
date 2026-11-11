//! Spike S1 driver (issue #3): shows the corral from Rust and exercises the C ABI. Spike code:
//! it stays on the spike branch and is not merged (the finding goes in docs/adr/001-stack.md).
//! Every line it prints starts with `corral-bridge:` so the run scripts can check the result.
//!
//! Step 1 (any build): roster, live updates, refusals, the tap round trip.
//! Step 2 (the packaged Corral.app only): a low-risk checkpoint notification with inline
//! Approve, then a critical one with no inline Approve whose Open leads to Touch ID. The
//! decisions (which buttons, whether Touch ID is needed, nonce checks) are made here in Rust;
//! Swift only shows what it's sent and reports clicks (AGENTS.md rule 3).

use crate::bridge::{
    self, AuthOutcome, AuthRequest, Intent, NoteAction, NotificationRequest, Offer,
};
use crate::native;
use corral_core::ids::{AgentId, CheckpointId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// Open checkpoints (single-use nonces) and Touch ID requests awaiting an answer.
#[derive(Default)]
struct Step2 {
    nonces: HashMap<CheckpointId, String>,
    auth_for: HashMap<String, CheckpointId>,
}

type State = Arc<Mutex<Step2>>;

/// Starts the corral with three fake agents, then runs the probes on a background thread.
/// Called from Tauri's `setup`, which runs on the main thread.
pub fn start(app: &AppHandle) -> Result<(), native::NativeError> {
    let t0 = Instant::now();
    let state: State = Arc::default();
    let (handler_app, handler_state) = (app.clone(), Arc::clone(&state));
    native::start(move |result| {
        let ms = t0.elapsed().as_millis();
        match result {
            Ok(intent) => {
                // The timestamp tells a double click (~100-300 ms apart) from a double fire (~0 ms).
                println!("corral-bridge: intent {intent:?} (+{ms} ms)");
                on_intent(&handler_app, &handler_state, intent);
            }
            Err(e) => println!("corral-bridge: refused intent: {e} (+{ms} ms)"),
        }
    })?;
    native::set_roster(&bridge::demo_roster(3))?;
    println!("corral-bridge: started; roster of 3 sent");
    let app = app.clone();
    std::thread::spawn(move || {
        step1(&app);
        if in_app_bundle() {
            std::thread::sleep(Duration::from_secs(2));
            step2(&app, &state);
        } else {
            println!(
                "corral-bridge: not running from Corral.app; skipping notifications and Touch ID"
            );
        }
    });
    Ok(())
}

fn in_app_bundle() -> bool {
    match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().contains(".app/Contents/MacOS/"),
        Err(e) => {
            println!(
                "corral-bridge: can't read the executable path ({e}); treating as a bare binary"
            );
            false
        }
    }
}

/// Runs `f` on the main thread, the only thread CorralNative accepts, and logs the outcome.
fn on_main(
    app: &AppHandle,
    label: String,
    f: impl FnOnce() -> Result<(), native::NativeError> + Send + 'static,
) {
    let shown = label.clone();
    let posted = app.run_on_main_thread(move || match f() {
        Ok(()) => println!("corral-bridge: {label}: ok"),
        Err(e) => println!("corral-bridge: {label}: {e}"),
    });
    if let Err(e) = posted {
        println!("corral-bridge: {shown}: couldn't reach the main thread: {e}");
    }
}

fn step1(app: &AppHandle) {
    std::thread::sleep(Duration::from_secs(3));
    on_main(app, "roster of 5 (live update)".into(), || {
        native::set_roster(&bridge::demo_roster(5))
    });
    std::thread::sleep(Duration::from_secs(2));
    on_main(app, "roster of 10 (8 shown, 2 hidden)".into(), || {
        native::set_roster(&bridge::demo_roster(10))
    });
    // Bad input is refused, never trapped on (TP-BRIDGE-03). Expect "rejected".
    on_main(app, "malformed roster".into(), || {
        native::set_roster_bytes(b"{\"v\":1,")
    });
    // Off the main thread Swift refuses and does nothing. Expect "must be called on the main thread".
    match native::set_roster(&bridge::demo_roster(1)) {
        Ok(()) => println!("corral-bridge: off-main roster: ok (unexpected)"),
        Err(e) => println!("corral-bridge: off-main roster: {e}"),
    }
    std::thread::sleep(Duration::from_secs(1));
    // The round trip without a click: Swift emits openAgent through the tap path.
    on_main(app, "probe openAgent(scout)".into(), || {
        native::probe_open_agent("scout")
    });
    println!("corral-bridge: probes done; click a drop to send openAgent from a real tap");
}

fn step2(app: &AppHandle, state: &State) {
    // Swift refuses buttons it doesn't know. Expect "rejected".
    on_main(app, "bad notification (unknown button)".into(), || {
        native::notify_bytes(
            br#"{"v":1,"checkpoint":"cp-x","nonce":"n-x","agent":"scout","title":"t","body":"b","actions":["approveAll"]}"#,
        )
    });
    post(
        app,
        state,
        ("cp-1", "n-1"),
        "notify cp-1 (low risk: Approve, Deny, Open)",
        NotificationRequest::new(
            CheckpointId::from("cp-1"),
            "n-1",
            AgentId::from("scout"),
            "Scout needs you",
            "Send the weekly summary email to 3 people?",
            vec![Offer::Approve, Offer::Deny, Offer::Open],
        ),
    );
    println!(
        "corral-bridge: step 2: on the Scout notification choose Approve (allow notifications first if asked)"
    );
}

/// Records the checkpoint's single-use nonce, then posts its notification.
fn post(
    app: &AppHandle,
    state: &State,
    (checkpoint, nonce): (&str, &str),
    label: &str,
    request: NotificationRequest,
) {
    match state.lock() {
        Ok(mut s) => {
            s.nonces
                .insert(CheckpointId::from(checkpoint), nonce.to_owned());
        }
        Err(_) => {
            println!("corral-bridge: spike state poisoned; not posting {checkpoint}");
            return;
        }
    }
    on_main(app, label.to_owned(), move || native::notify(&request));
}

fn on_intent(app: &AppHandle, state: &State, intent: Intent) {
    match intent {
        Intent::NotificationAction {
            checkpoint,
            nonce,
            action,
        } => {
            // Single use: the nonce is spent whatever the user chose.
            let expected = match state.lock() {
                Ok(mut s) => s.nonces.remove(&checkpoint),
                Err(_) => {
                    println!("corral-bridge: spike state poisoned; intent ignored");
                    return;
                }
            };
            match expected {
                None => {
                    println!(
                        "corral-bridge: checkpoint {checkpoint}: {action:?} refused (not open)"
                    );
                    return;
                }
                Some(expected) if expected != nonce => {
                    println!(
                        "corral-bridge: checkpoint {checkpoint}: {action:?} refused (stale nonce)"
                    );
                    return;
                }
                Some(_) => {}
            }
            match (checkpoint.as_str(), action) {
                ("cp-1", NoteAction::Approve) => {
                    println!("corral-bridge: checkpoint cp-1: approved (nonce ok)");
                    critical_checkpoint(app, state);
                }
                ("cp-1", other) => {
                    println!("corral-bridge: checkpoint cp-1: {other:?} (expected Approve)");
                    critical_checkpoint(app, state);
                }
                ("cp-2", NoteAction::Open) => {
                    println!(
                        "corral-bridge: checkpoint cp-2: opened; critical, so approving needs Touch ID"
                    );
                    match state.lock() {
                        Ok(mut s) => {
                            s.auth_for.insert("auth-1".into(), checkpoint);
                        }
                        Err(_) => {
                            println!("corral-bridge: spike state poisoned; no Touch ID");
                            return;
                        }
                    }
                    on_main(app, "Touch ID auth-1".into(), || {
                        native::authenticate(&AuthRequest::new(
                            "auth-1",
                            "approve Scout spending $4.00",
                        ))
                    });
                }
                (cp, other) => {
                    println!("corral-bridge: checkpoint {cp}: {other:?}");
                    println!("corral-bridge: step 2 complete");
                }
            }
        }
        Intent::NotificationFailed { checkpoint, reason } => {
            println!("corral-bridge: notification {checkpoint} failed: {reason:?}");
            println!("corral-bridge: step 2 complete");
        }
        Intent::AuthResult { request, outcome } => {
            let checkpoint = match state.lock() {
                Ok(mut s) => s.auth_for.remove(&request),
                Err(_) => None,
            };
            println!("corral-bridge: auth {request}: {outcome:?}");
            match (checkpoint, outcome) {
                (Some(cp), AuthOutcome::Ok) => {
                    println!("corral-bridge: checkpoint {cp}: approved with Touch ID");
                }
                (Some(cp), other) => {
                    println!("corral-bridge: checkpoint {cp}: not approved ({other:?})");
                }
                (None, _) => {
                    println!("corral-bridge: auth {request}: no checkpoint waiting (ignored)");
                }
            }
            println!("corral-bridge: step 2 complete");
        }
        Intent::OpenAgent(_) | Intent::Deploy(_) | Intent::SummonPromptBar => {}
    }
}

/// The critical checkpoint: Rust offers no inline Approve, so the only way to approve is Open,
/// then Touch ID.
fn critical_checkpoint(app: &AppHandle, state: &State) {
    post(
        app,
        state,
        ("cp-2", "n-2"),
        "notify cp-2 (critical: Deny, Open only)",
        NotificationRequest::new(
            CheckpointId::from("cp-2"),
            "n-2",
            AgentId::from("scout"),
            "Scout wants to spend $4.00",
            "Critical: approving needs Touch ID. Choose Open.",
            vec![Offer::Deny, Offer::Open],
        ),
    );
    println!("corral-bridge: step 2: on the spend notification choose Open, then touch the sensor");
}
