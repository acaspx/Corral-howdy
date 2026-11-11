// CorralCheckpointWire.swift
// Checkpoint notifications and Touch ID over the C ABI (ADR-001, docs/01 §5 and §7).
// Pure Foundation, unit-tested against the shared Fixtures/bridge files.
//
// The split, per AGENTS.md rule 3: Rust decides which buttons a checkpoint notification offers
// (high and critical risks get no inline Approve, docs/01 "Notifications") and when Touch ID is
// required (critical, TP-POL-09). Swift only draws what it is sent and reports what the user did,
// carrying the checkpoint's single-use nonce back so Rust can refuse a stale approval (T18).

import Foundation
import UserNotifications

/// Why a payload from Rust was refused. Nothing here traps.
public enum CorralWireError: Error, Equatable, Sendable {
    case empty
    case tooLarge(Int)
    case malformed
    case unsupportedVersion(Int)
    /// The named field was empty, too long, had control characters or an unknown value.
    case invalid(String)
}

/// What `corral_native_notify` receives (`notify-v1.json`).
public struct CorralNotificationRequest: Equatable, Sendable {
    public enum Action: String, Sendable, CaseIterable {
        case approve, deny, open
    }

    public static let wireVersion = 1
    public static let maxBytes = 16 * 1024
    public static let maxTitleLength = 120
    public static let maxBodyLength = 600

    public var checkpoint: String
    public var nonce: String
    public var agent: String
    public var title: String
    public var body: String
    /// The buttons to offer, in order. Exactly what Rust sent, deduped.
    public var actions: [Action]

    public static func decode(_ data: Data) -> Result<CorralNotificationRequest, CorralWireError> {
        guard !data.isEmpty else { return .failure(.empty) }
        guard data.count <= maxBytes else { return .failure(.tooLarge(data.count)) }
        let wire: Wire
        do { wire = try JSONDecoder().decode(Wire.self, from: data) } catch { return .failure(.malformed) }
        guard wire.v == wireVersion else { return .failure(.unsupportedVersion(wire.v)) }
        guard CorralRoster.isValidID(wire.checkpoint) else { return .failure(.invalid("checkpoint")) }
        guard CorralRoster.isValidID(wire.nonce) else { return .failure(.invalid("nonce")) }
        guard CorralRoster.isValidID(wire.agent) else { return .failure(.invalid("agent")) }
        let title = CorralRoster.sanitize(wire.title, max: maxTitleLength)
        guard !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return .failure(.invalid("title")) }
        var actions: [Action] = []
        for raw in wire.actions {
            guard let action = Action(rawValue: raw) else { return .failure(.invalid("actions")) }
            if !actions.contains(action) { actions.append(action) }
        }
        guard !actions.isEmpty else { return .failure(.invalid("actions")) }
        return .success(CorralNotificationRequest(
            checkpoint: wire.checkpoint, nonce: wire.nonce, agent: wire.agent, title: title,
            body: CorralRoster.sanitize(wire.body, max: maxBodyLength), actions: actions))
    }

    /// One notification category per button set, registered up front so the buttons are there on
    /// the first notification (registering a category just before posting races).
    public static func categoryID(for actions: [Action]) -> String {
        "corral.cp." + actions.map(\.rawValue).joined(separator: "-")
    }

    /// Every ordered button set Rust can send, as category IDs. Order matters to the user, so
    /// each ordering is its own category (15 for three buttons).
    static var allCategoryActionSets: [[Action]] {
        func permutations(_ items: [Action]) -> [[Action]] {
            guard !items.isEmpty else { return [[]] }
            return items.indices.flatMap { i -> [[Action]] in
                var rest = items; let head = rest.remove(at: i)
                return permutations(rest).map { [head] + $0 }
            }
        }
        var sets: [[Action]] = []
        for mask in 1..<(1 << Action.allCases.count) {
            let subset = Action.allCases.enumerated().filter { mask & (1 << $0.offset) != 0 }.map(\.element)
            sets += permutations(subset)
        }
        return sets
    }

    private struct Wire: Decodable {
        let v: Int, checkpoint: String, nonce: String, agent: String, title: String, body: String
        let actions: [String]

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["v", "checkpoint", "nonce", "agent", "title", "body", "actions"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            v = try c.decode(Int.self, forKey: AnyKey("v"))
            checkpoint = try c.decode(String.self, forKey: AnyKey("checkpoint"))
            nonce = try c.decode(String.self, forKey: AnyKey("nonce"))
            agent = try c.decode(String.self, forKey: AnyKey("agent"))
            title = try c.decode(String.self, forKey: AnyKey("title"))
            body = try c.decode(String.self, forKey: AnyKey("body"))
            actions = try c.decode([String].self, forKey: AnyKey("actions"))
        }
    }
}

/// What `corral_native_authenticate` receives (`authenticate-v1.json`).
public struct CorralAuthRequest: Equatable, Sendable {
    public static let wireVersion = 1
    public static let maxBytes = 4 * 1024
    /// macOS shows it as "Corral is trying to <reason>".
    public static let maxReasonLength = 160

    public var request: String
    public var reason: String

    public static func decode(_ data: Data) -> Result<CorralAuthRequest, CorralWireError> {
        guard !data.isEmpty else { return .failure(.empty) }
        guard data.count <= maxBytes else { return .failure(.tooLarge(data.count)) }
        let wire: Wire
        do { wire = try JSONDecoder().decode(Wire.self, from: data) } catch { return .failure(.malformed) }
        guard wire.v == wireVersion else { return .failure(.unsupportedVersion(wire.v)) }
        guard CorralRoster.isValidID(wire.request) else { return .failure(.invalid("request")) }
        let reason = CorralRoster.sanitize(wire.reason, max: maxReasonLength)
        guard !reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return .failure(.invalid("reason")) }
        return .success(CorralAuthRequest(request: wire.request, reason: reason))
    }

    private struct Wire: Decodable {
        let v: Int, request: String, reason: String
        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["v", "request", "reason"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            v = try c.decode(Int.self, forKey: AnyKey("v"))
            request = try c.decode(String.self, forKey: AnyKey("request"))
            reason = try c.decode(String.self, forKey: AnyKey("reason"))
        }
    }
}

/// Maps a notification response to what the user did. Anything not Corral's own is ignored.
public enum CorralNotificationResponse: String, Sendable {
    case approve, deny, open, dismiss

    public static func action(for identifier: String) -> CorralNotificationResponse? {
        switch identifier {
        case "corral.approve": .approve
        case "corral.deny": .deny
        case "corral.open", UNNotificationDefaultActionIdentifier: .open
        case UNNotificationDismissActionIdentifier: .dismiss
        default: nil
        }
    }
}

public enum CorralNotificationFailure: String, Sendable {
    case notAuthorized, failed
}

public enum CorralAuthOutcome: String, Sendable {
    /// The user authenticated with Touch ID.
    case ok
    /// The user cancelled, or the system did (another app, a timeout).
    case cancelled
    /// Touch ID didn't match.
    case failed
    /// No Touch ID on this Mac, or none enrolled. Rust decides the fallback.
    case unavailable
}

extension CorralIntentWire {
    private struct CheckpointEnvelope: Encodable {
        let v: Int
        let intent: CheckpointBody
    }

    private struct CheckpointBody: Encodable {
        let kind: String
        var checkpoint: String?
        var nonce: String?
        var action: String?
        var reason: String?
        var request: String?
        var outcome: String?
    }

    private static func encodeCheckpoint(_ body: CheckpointBody) -> Data? {
        try? JSONEncoder().encode(CheckpointEnvelope(v: wireVersion, intent: body))
    }

    public static func encodeNotificationAction(checkpoint: String, nonce: String, action: CorralNotificationResponse) -> Data? {
        encodeCheckpoint(CheckpointBody(kind: "notificationAction", checkpoint: checkpoint, nonce: nonce, action: action.rawValue))
    }

    public static func encodeNotificationFailed(checkpoint: String, reason: CorralNotificationFailure) -> Data? {
        encodeCheckpoint(CheckpointBody(kind: "notificationFailed", checkpoint: checkpoint, reason: reason.rawValue))
    }

    public static func encodeAuthResult(request: String, outcome: CorralAuthOutcome) -> Data? {
        encodeCheckpoint(CheckpointBody(kind: "authResult", request: request, outcome: outcome.rawValue))
    }
}
