// CorralBridge.swift
// The narrow C ABI between the Rust core and CorralNative (Spike S1, docs/01 §2).
//
//   Rust → Swift   corral_native_abi_version, corral_native_start, corral_native_set_roster,
//                  corral_native_stop, corral_native_probe_open_agent (spike probe)
//   Swift → Rust   one callback: (pointer, length) of a JSON intent (Fixtures/bridge/intent-*-v1.json)
//
// Rules for every entry point:
//   - Main thread only. Called from any other thread it returns `notMainThread` and does nothing;
//     it never hops threads on Rust's behalf, so ordering stays Rust's job.
//   - Returns a status code; never traps on bad input (TP-BRIDGE-03). Payloads are JSON bytes,
//     so the ABI carries no Swift or Rust types and needs no shared headers.
//   - The intent callback's bytes are only valid during the call; Rust copies them.
//   - Swift reports what the user did. It never decides anything (AGENTS.md rule 3).

import AppKit
import SwiftUI

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
public let corralNativeABIVersion: UInt32 = 1

@MainActor
final class CorralBridgeModel: ObservableObject {
    @Published var agents: [CorralAgentToken] = []
    @Published var hiddenCount = 0
}

struct CorralBridgeRoot: View {
    @ObservedObject var model: CorralBridgeModel
    var body: some View {
        CorralGlassView(agents: model.agents, hiddenCount: model.hiddenCount) { intent in
            MainActor.assumeIsolated { CorralBridge.emit(intent) }
        }
    }
}

@MainActor
enum CorralBridge {
    static var panel: CorralPanel?
    static var callback: CorralIntentCallback?
    static let model = CorralBridgeModel()
    private static var screenObserver: NSObjectProtocol?

    static func start(_ callback: CorralIntentCallback) -> CorralBridgeStatus {
        guard panel == nil else { return .alreadyStarted }
        self.callback = callback
        let panel = CorralPanel(rootView: CorralBridgeRoot(model: model))
        self.panel = panel
        place()
        panel.orderFrontRegardless()
        // Displays plugged in, unplugged or rearranged: put the corral back at the bottom centre.
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { CorralBridge.place() }
        }
        return .ok
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
            return .ok
        }
    }

    static func stop() -> CorralBridgeStatus {
        guard let panel else { return .notStarted }
        panel.orderOut(nil)
        self.panel = nil
        callback = nil
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
        screenObserver = nil
        return .ok
    }

    /// Spike probe: emits `openAgent` for an agent that is in the corral, through the same path a
    /// tap uses, so a script can check the round trip without a click. Not for production.
    static func probeOpenAgent(_ data: Data) -> CorralBridgeStatus {
        guard panel != nil else { return .notStarted }
        guard let id = String(data: data, encoding: .utf8),
              model.agents.contains(where: { $0.id == id }) else { return .rejected }
        emit(.openAgent(id: id))
        return .ok
    }

    static func emit(_ intent: CorralIntent) {
        guard let callback, let data = CorralIntentWire.encode(intent) else { return }
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

@_cdecl("corral_native_stop")
public func corral_native_stop() -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    return MainActor.assumeIsolated { CorralBridge.stop().rawValue }
}

@_cdecl("corral_native_probe_open_agent")
public func corral_native_probe_open_agent(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard Thread.isMainThread else { return CorralBridgeStatus.notMainThread.rawValue }
    switch borrowed(bytes, count, limit: CorralRoster.maxIDLength * 4) {
    case .failure(let status): return status.rawValue
    case .success(let data): return MainActor.assumeIsolated { CorralBridge.probeOpenAgent(data).rawValue }
    }
}
