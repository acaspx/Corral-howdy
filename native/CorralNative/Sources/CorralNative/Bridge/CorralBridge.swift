// CorralBridge.swift
// The narrow C ABI between the Rust core and CorralNative (ADR-001, docs/01 §2).
//
//   Rust → Swift   corral_native_abi_version, corral_native_start, corral_native_set_roster,
//                  corral_native_notify, corral_native_authenticate, corral_native_stop
//   Swift → Rust   one callback: (pointer, length) of a JSON intent (Fixtures/bridge/intent-*-v1.json)
//
// Notifications and Touch ID need a real app bundle: UNUserNotificationCenter refuses to work
// for a bare executable, so they only work from the packaged Corral.app.
//
// Rules for every entry point:
//   - Main thread only. Called from any other thread it returns `notMainThread` and does nothing;
//     it never hops threads on Rust's behalf, so ordering stays Rust's job.
//   - Returns a status code; never traps on bad input (TP-BRIDGE-03). Payloads are JSON bytes,
//     so the ABI carries no Swift or Rust types and needs no shared headers.
//   - The intent callback's bytes are only valid during the call; Rust copies them.
//   - Swift reports what the user did. It never decides anything (AGENTS.md rule 3).

import AppKit
import LocalAuthentication
import SwiftUI
import UserNotifications

/// `void (*)(const uint8_t *bytes, intptr_t len)`
public typealias CorralIntentCallback = @convention(c) (UnsafePointer<UInt8>?, Int) -> Void

/// Status codes returned across the ABI. Rust mirrors these in `bridge::Status`.
/// `Error` so a refusal can travel in a `Result` inside Swift.
public enum CorralBridgeStatus: Int32, Error, Sendable {
    case ok = 0
    case notMainThread = 1
    case alreadyStarted = 2
    case notStarted = 3
    case rejected = 4
    case nullArgument = 5
}

/// Bump when any signature or payload above changes. Rust refuses to start on a mismatch.
public let corralNativeABIVersion: UInt32 = 2

@MainActor
final class CorralBridgeModel: ObservableObject {
    @Published var agents: [CorralAgentToken] = []
    @Published var hiddenCount = 0
}

struct CorralBridgeRoot: View {
    @ObservedObject var model: CorralBridgeModel
    var body: some View {
        CorralGlassView(agents: model.agents, hiddenCount: model.hiddenCount, onIntent: { intent in
            MainActor.assumeIsolated {
                // A click on a drop opens its card here; the card's buttons and field talk to Rust.
                if case .openAgent(let id) = intent {
                    CorralBridge.cards?.dropClicked(id)
                } else {
                    CorralBridge.emit(intent)
                }
            }
        }, onHoverDrop: { id, inside in
            MainActor.assumeIsolated { CorralBridge.cards?.swiftUIHover(drop: id, inside: inside) }
        })
    }
}

@MainActor
enum CorralBridge {
    static var panel: CorralPanel?
    /// Each drop's card (SK-6): hover, pin, the task field and the buttons.
    static var cards: CorralCardController?
    static var callback: CorralIntentCallback?
    static let model = CorralBridgeModel()
    private static var screenObserver: NSObjectProtocol?
    private static let notificationDelegate = CorralNotificationDelegate()
    /// The Touch ID prompt that's up, if any. A second request is refused rather than queued.
    private static var authContext: LAContext?

    static func start(_ callback: CorralIntentCallback) -> CorralBridgeStatus {
        guard panel == nil else { return .alreadyStarted }
        self.callback = callback
        let panel = CorralPanel(rootView: CorralBridgeRoot(model: model))
        self.panel = panel
        cards = CorralCardController(corralPanel: panel) { intent in
            MainActor.assumeIsolated { CorralBridge.emit(intent) }
        }
        place()
        panel.orderFrontRegardless()
        // Displays plugged in, unplugged or rearranged: put the corral back at the bottom centre.
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { CorralBridge.place() }
        }
        // The delegate is set at launch so a click on a notification that launched the app is seen.
        // UNUserNotificationCenter traps in a bare executable, so it's only touched inside a .app.
        if inAppBundle {
            let center = UNUserNotificationCenter.current()
            center.delegate = notificationDelegate
            center.setNotificationCategories(Set(CorralNotificationRequest.allCategoryActionSets.map(category)))
        }
        return .ok
    }

    /// UNUserNotificationCenter traps outside a real .app on disk. An Info.plist embedded in a
    /// bare binary (Tauri does that in dev) gives a bundle identifier but isn't enough.
    static var inAppBundle: Bool {
        Bundle.main.bundleURL.pathExtension == "app" && Bundle.main.bundleIdentifier != nil
    }

    private static func category(_ actions: [CorralNotificationRequest.Action]) -> UNNotificationCategory {
        let buttons = actions.map { action -> UNNotificationAction in
            switch action {
            // Approve from a notification still needs the Mac unlocked; Rust adds Touch ID on top
            // for critical risks, and never offers inline Approve for high or critical.
            case .approve: UNNotificationAction(identifier: "corral.approve", title: "Approve", options: [.authenticationRequired])
            case .deny: UNNotificationAction(identifier: "corral.deny", title: "Deny", options: [.destructive])
            case .open: UNNotificationAction(identifier: "corral.open", title: "Open", options: [.foreground])
            }
        }
        return UNNotificationCategory(identifier: CorralNotificationRequest.categoryID(for: actions),
                                      actions: buttons, intentIdentifiers: [], options: [.customDismissAction])
    }

    static func notify(_ data: Data) -> CorralBridgeStatus {
        guard panel != nil else { return .notStarted }
        guard inAppBundle else { return .rejected }
        let request: CorralNotificationRequest
        switch CorralNotificationRequest.decode(data) {
        case .failure(let error):
            NSLog("CorralNative: notification rejected (%@)", String(describing: error))
            return .rejected
        case .success(let r): request = r
        }
        // Completion-handler APIs. The closures are explicitly @Sendable: without that, Swift 6
        // infers them main-actor isolated and traps when the system calls them on its own queue.
        // They carry only Sendable values and hop to the main thread before emitting.
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert, .sound]) { @Sendable granted, error in
            guard granted, error == nil else {
                CorralBridge.onMain { CorralBridge.emitNotificationFailed(request.checkpoint, .notAuthorized) }
                return
            }
            let content = UNMutableNotificationContent()
            content.title = request.title
            content.body = request.body
            content.sound = .default
            content.categoryIdentifier = CorralNotificationRequest.categoryID(for: request.actions)
            content.userInfo = ["checkpoint": request.checkpoint, "nonce": request.nonce]
            let note = UNNotificationRequest(identifier: "corral.checkpoint.\(request.checkpoint)", content: content, trigger: nil)
            UNUserNotificationCenter.current().add(note) { @Sendable error in
                guard let error else { return }
                NSLog("CorralNative: notification failed (%@)", String(describing: error))
                CorralBridge.onMain { CorralBridge.emitNotificationFailed(request.checkpoint, .failed) }
            }
        }
        return .ok
    }

    static func authenticate(_ data: Data) -> CorralBridgeStatus {
        guard panel != nil else { return .notStarted }
        guard authContext == nil else { return .rejected }
        let request: CorralAuthRequest
        switch CorralAuthRequest.decode(data) {
        case .failure(let error):
            NSLog("CorralNative: Touch ID request rejected (%@)", String(describing: error))
            return .rejected
        case .success(let r): request = r
        }
        let context = LAContext()
        context.localizedFallbackTitle = ""          // Touch ID only: no "Enter Password" button
        var error: NSError?
        // Touch ID only: a critical approval needs the finger, not the login password. Without
        // Touch ID, Rust is told `unavailable` and decides the fallback (ADR-001 open question).
        guard context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &error) else {
            emit(data: CorralIntentWire.encodeAuthResult(request: request.request, outcome: .unavailable))
            return .ok
        }
        authContext = context                        // kept alive until the reply
        context.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, localizedReason: request.reason) { @Sendable success, error in
            let outcome = success ? CorralAuthOutcome.ok : CorralBridge.outcome(for: error)
            CorralBridge.onMain {
                CorralBridge.authContext = nil
                CorralBridge.emit(data: CorralIntentWire.encodeAuthResult(request: request.request, outcome: outcome))
            }
        }
        return .ok
    }

    nonisolated static func outcome(for error: (any Error)?) -> CorralAuthOutcome {
        guard let error = error as? LAError else { return .failed }
        switch error.code {
        case .userCancel, .systemCancel, .appCancel, .userFallback: return .cancelled
        case .biometryNotAvailable, .biometryNotEnrolled, .passcodeNotSet: return .unavailable
        default: return .failed
        }
    }

    /// Runs `work` on the main thread from a system callback on any queue.
    nonisolated static func onMain(_ work: @escaping @MainActor @Sendable () -> Void) {
        DispatchQueue.main.async { MainActor.assumeIsolated { work() } }
    }

    static func emitNotificationAction(checkpoint: String, nonce: String, action: CorralNotificationResponse) {
        emit(data: CorralIntentWire.encodeNotificationAction(checkpoint: checkpoint, nonce: nonce, action: action))
    }

    static func emitNotificationFailed(_ checkpoint: String, _ reason: CorralNotificationFailure) {
        emit(data: CorralIntentWire.encodeNotificationFailed(checkpoint: checkpoint, reason: reason))
    }

    static func setRoster(_ data: Data) -> CorralBridgeStatus {
        guard panel != nil else { return .notStarted }
        switch CorralRoster.decode(data) {
        case .failure(let error):
            NSLog("CorralNative: roster rejected (%@); keeping the previous one", String(describing: error))
            return .rejected
        case .success(let roster):
            model.agents = roster.agents
            model.hiddenCount = roster.hiddenCount
            place()
            cards?.update(agents: roster.agents, cards: roster.cards)
            return .ok
        }
    }

    static func stop() -> CorralBridgeStatus {
        guard let panel else { return .notStarted }
        cards?.stop()
        cards = nil
        panel.orderOut(nil)
        self.panel = nil
        callback = nil
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
        screenObserver = nil
        return .ok
    }

    static func emit(_ intent: CorralIntent) {
        emit(data: CorralIntentWire.encode(intent))
    }

    static func emit(data: Data?) {
        guard let callback, let data else { return }
        data.withUnsafeBytes { raw in
            callback(raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
        }
    }

    static func place() {
        panel?.place(canvas: CorralLayout.layout(rankedIDs: model.agents.map(\.id)).canvas)
    }
}

// MARK: C entry points

/// Copies `count` bytes from Rust, refusing null pointers and anything over `limit` before copying.
private func borrowed(_ bytes: UnsafePointer<UInt8>?, _ count: Int, limit: Int) -> Result<Data, CorralBridgeStatus> {
    guard let bytes, count >= 0 else { return .failure(.nullArgument) }
    guard count <= limit else { return .failure(.rejected) }
    return .success(Data(bytes: bytes, count: count))
}

@_cdecl("corral_native_abi_version")
public func corral_native_abi_version() -> UInt32 { corralNativeABIVersion }

@_cdecl("corral_native_start")
public func corral_native_start(_ callback: CorralIntentCallback?) -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    guard let callback else { return CorralBridgeStatus.nullArgument.rawValue }
    return MainActor.assumeIsolated { CorralBridge.start(callback).rawValue }
}

@_cdecl("corral_native_set_roster")
public func corral_native_set_roster(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    switch borrowed(bytes, count, limit: CorralRoster.maxBytes) {
    case .failure(let status): return status.rawValue
    case .success(let data): return MainActor.assumeIsolated { CorralBridge.setRoster(data).rawValue }
    }
}

@_cdecl("corral_native_notify")
public func corral_native_notify(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    switch borrowed(bytes, count, limit: CorralNotificationRequest.maxBytes) {
    case .failure(let status): return status.rawValue
    case .success(let data): return MainActor.assumeIsolated { CorralBridge.notify(data).rawValue }
    }
}

@_cdecl("corral_native_authenticate")
public func corral_native_authenticate(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    switch borrowed(bytes, count, limit: CorralAuthRequest.maxBytes) {
    case .failure(let status): return status.rawValue
    case .success(let data): return MainActor.assumeIsolated { CorralBridge.authenticate(data).rawValue }
    }
}

@_cdecl("corral_native_stop")
public func corral_native_stop() -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    return MainActor.assumeIsolated { CorralBridge.stop().rawValue }
}

// MARK: Notification responses

/// Receives the user's click on a Corral notification and reports it to Rust. Holds no state.
final class CorralNotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    /// Show checkpoint notifications even while Corral is the active app.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            didReceive response: UNNotificationResponse) async {
        let info = response.notification.request.content.userInfo
        // Only Corral's own notifications carry these; anything else is ignored.
        guard let checkpoint = info["checkpoint"] as? String, CorralRoster.isValidID(checkpoint),
              let nonce = info["nonce"] as? String, CorralRoster.isValidID(nonce),
              let action = CorralNotificationResponse.action(for: response.actionIdentifier) else { return }
        await MainActor.run {
            CorralBridge.emitNotificationAction(checkpoint: checkpoint, nonce: nonce, action: action)
        }
    }
}
