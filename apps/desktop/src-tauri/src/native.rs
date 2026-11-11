//! The C ABI into CorralNative (Swift), ADR-001. macOS only, behind the `native` feature.
//!
//! This is the only `unsafe` in the shell. Every block is a call into the Swift entry points
//! in `native/CorralNative/Sources/CorralNative/Bridge/CorralBridge.swift`, or a read of bytes Swift
//! lends to the intent callback. Payloads are JSON (`bridge.rs`), so no Rust or Swift type crosses.
//! ADR-001 (`docs/adr/001-stack.md`) is the ADR AGENTS.md requires for this `unsafe`.

use crate::bridge::{
    self, AuthRequest, BridgeError, Intent, NotificationRequest, RosterSnapshot, Status,
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

type IntentCallback = extern "C" fn(*const u8, isize);

// SAFETY (for the whole block): these symbols are `@_cdecl` functions in CorralNative, linked
// statically by build.rs. Their signatures match `CorralBridge.swift`, and the version check in
// `start` refuses to run against a different ABI.
unsafe extern "C" {
    fn corral_native_abi_version() -> u32;
    fn corral_native_start(callback: Option<IntentCallback>) -> i32;
    fn corral_native_set_roster(bytes: *const u8, len: isize) -> i32;
    fn corral_native_notify(bytes: *const u8, len: isize) -> i32;
    fn corral_native_authenticate(bytes: *const u8, len: isize) -> i32;
    fn corral_native_stop() -> i32;
}

/// Why the corral surface couldn't do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The linked CorralNative speaks a different ABI.
    #[error("CorralNative ABI {found}; this build expects {expected}")]
    AbiMismatch {
        /// What Rust was built against.
        expected: u32,
        /// What the linked Swift reports.
        found: u32,
    },
    /// Swift returned a non-success status.
    #[error(transparent)]
    Status(#[from] Status),
    /// The payload couldn't be built.
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    /// The intent handler was already installed by an earlier `start`.
    #[error("the intent handler is already installed")]
    HandlerInstalled,
}

type Handler = Box<dyn Fn(Result<Intent, BridgeError>) + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

/// Called by Swift on the main thread for every user intent. Never unwinds into Swift.
extern "C" fn on_intent(bytes: *const u8, len: isize) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let result = if bytes.is_null() {
            Err(BridgeError::Null)
        } else {
            match usize::try_from(len) {
                Err(_) => Err(BridgeError::TooLarge(0)),
                Ok(len) if len > bridge::MAX_INTENT_BYTES => Err(BridgeError::TooLarge(len)),
                Ok(len) => {
                    // SAFETY: Swift's `CorralBridge.emit` passes a pointer to `len` initialised
                    // bytes that stay valid until this call returns. They are copied first.
                    let bytes = unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec();
                    bridge::decode_intent(&bytes)
                }
            }
        };
        if let Some(handler) = HANDLER.get() {
            handler(result);
        }
    }));
    if outcome.is_err() {
        eprintln!("corral-bridge: the intent handler panicked; the intent was dropped");
    }
}

/// Shows the corral panel and installs `handler` for intents. Main thread only.
pub fn start(
    handler: impl Fn(Result<Intent, BridgeError>) + Send + Sync + 'static,
) -> Result<(), NativeError> {
    // SAFETY: no arguments; returns a constant.
    let found = unsafe { corral_native_abi_version() };
    if found != bridge::ABI_VERSION {
        return Err(NativeError::AbiMismatch {
            expected: bridge::ABI_VERSION,
            found,
        });
    }
    HANDLER
        .set(Box::new(handler))
        .map_err(|_| NativeError::HandlerInstalled)?;
    // SAFETY: `on_intent` is a valid `extern "C"` function for the life of the process.
    Status::check(unsafe { corral_native_start(Some(on_intent)) })?;
    Ok(())
}

/// Replaces the roster the corral shows. Main thread only (Swift refuses otherwise).
pub fn set_roster(snapshot: &RosterSnapshot) -> Result<(), NativeError> {
    let bytes = snapshot.to_bytes()?;
    let len = isize::try_from(bytes.len()).map_err(|_| Status::Rejected)?;
    // SAFETY: `bytes` is valid for `len` bytes for the duration of the call; Swift copies them.
    Status::check(unsafe { corral_native_set_roster(bytes.as_ptr(), len) })?;
    Ok(())
}

/// Posts a checkpoint notification. Main thread only, and only from the packaged .app (Swift
/// refuses in a bare binary). Clicks come back as [`Intent::NotificationAction`].
pub fn notify(request: &NotificationRequest) -> Result<(), NativeError> {
    let bytes = request.to_bytes()?;
    let len = isize::try_from(bytes.len()).map_err(|_| Status::Rejected)?;
    // SAFETY: `bytes` is valid for `len` bytes for the duration of the call; Swift copies them.
    Status::check(unsafe { corral_native_notify(bytes.as_ptr(), len) })?;
    Ok(())
}

/// Shows a Touch ID prompt. Main thread only. The result comes back as [`Intent::AuthResult`].
pub fn authenticate(request: &AuthRequest) -> Result<(), NativeError> {
    let bytes = request.to_bytes()?;
    let len = isize::try_from(bytes.len()).map_err(|_| Status::Rejected)?;
    // SAFETY: `bytes` is valid for `len` bytes for the duration of the call; Swift copies them.
    Status::check(unsafe { corral_native_authenticate(bytes.as_ptr(), len) })?;
    Ok(())
}

/// Hides the corral. Main thread only.
pub fn stop() -> Result<(), NativeError> {
    // SAFETY: no arguments.
    Status::check(unsafe { corral_native_stop() })?;
    Ok(())
}
