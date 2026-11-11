// CorralRoster.swift
// The wire contract with the Rust core over the C ABI (Spike S1, docs/01 §2): the roster snapshot
// Rust sends, and the intents Swift sends back. Pure Foundation, so it is unit-tested
// (Tests/CorralNativeTests/CorralBridgeTests.swift) against the shared Fixtures/bridge files.
//
// Everything from Rust is untrusted (AGENTS.md, rules learned 2026-11-07): dedupe it, bound it,
// strip control characters, and never trap on it (TP-BRIDGE-03). A roster that can't be read is
// rejected whole and the corral keeps showing the previous one.

import Foundation

public struct CorralRoster: Equatable, Sendable {
    public static let wireVersion = 1
    /// Largest roster accepted, checked before the bytes are copied or parsed.
    public static let maxBytes = 64 * 1024
    /// The corral never shows more than an octagon (docs/07 §3).
    public static let maxShown = 8
    public static let maxIDLength = 64
    public static let maxNameLength = 64
    public static let maxMonogramLength = 3
    public static let maxHidden = 9_999

    /// Agents to show, most used first, at most `maxShown`.
    public var agents: [CorralAgentToken]
    /// Agents that exist but aren't shown. Spoken by VoiceOver only.
    public var hiddenCount: Int

    public enum DecodeError: Error, Equatable, Sendable {
        case empty
        case tooLarge(Int)
        case malformed
        case unsupportedVersion(Int)
        case unknownStatus(String)
    }

    public static func decode(_ data: Data) -> Result<CorralRoster, DecodeError> {
        guard !data.isEmpty else { return .failure(.empty) }
        guard data.count <= maxBytes else { return .failure(.tooLarge(data.count)) }
        let wire: Wire
        do { wire = try JSONDecoder().decode(Wire.self, from: data) } catch { return .failure(.malformed) }
        guard wire.v == wireVersion else { return .failure(.unsupportedVersion(wire.v)) }

        var seen = Set<String>()
        var shown: [CorralAgentToken] = []
        var overflow = 0
        for agent in wire.agents {
            guard let status = Self.status(agent.status) else {
                return .failure(.unknownStatus(sanitize(agent.status, max: 32)))
            }
            // IDs are identity, so a bad one drops the agent rather than being rewritten:
            // truncating could make two different agents collide.
            guard isValidID(agent.id), seen.insert(agent.id).inserted else { continue }
            guard shown.count < maxShown else { overflow += 1; continue }
            let name = sanitize(agent.name, max: maxNameLength)
            var monogram = sanitize(agent.monogram, max: maxMonogramLength)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if monogram.isEmpty { monogram = name.first.map { String($0).uppercased() } ?? "?" }
            shown.append(CorralAgentToken(id: agent.id, name: name, monogram: monogram, status: status))
        }
        let hidden = min(max(wire.hidden, 0), maxHidden) + overflow
        return .success(CorralRoster(agents: shown, hiddenCount: min(hidden, maxHidden)))
    }

    static func status(_ s: String) -> CorralAgentToken.Status? {
        switch s {
        case "available": .available
        case "working": .working
        case "needsYou": .needsYou
        case "done": .done
        case "error": .error
        default: nil
        }
    }

    static func isValidID(_ id: String) -> Bool {
        !id.isEmpty && id.count <= maxIDLength
            && !id.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) || CharacterSet.whitespacesAndNewlines.contains($0) }
    }

    /// Removes control and format characters (including bidi overrides) and keeps at most `max` characters.
    static func sanitize(_ s: String, max: Int) -> String {
        var scalars = String.UnicodeScalarView()
        scalars.append(contentsOf: s.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) })
        return String(String(scalars).prefix(max))
    }

    // MARK: Wire format (Fixtures/bridge/roster-v1.json). Unknown keys are rejected, like Rust's
    // `deny_unknown_fields`, so the two sides can't silently drift apart.

    private struct Wire: Decodable {
        let v: Int
        let agents: [Agent]
        let hidden: Int

        struct Agent: Decodable {
            let id: String, name: String, monogram: String, status: String
            init(from decoder: Decoder) throws {
                try rejectUnknownKeys(decoder, allowed: ["id", "name", "monogram", "status"])
                let c = try decoder.container(keyedBy: AnyKey.self)
                id = try c.decode(String.self, forKey: AnyKey("id"))
                name = try c.decode(String.self, forKey: AnyKey("name"))
                monogram = try c.decode(String.self, forKey: AnyKey("monogram"))
                status = try c.decode(String.self, forKey: AnyKey("status"))
            }
        }

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["v", "agents", "hidden"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            v = try c.decode(Int.self, forKey: AnyKey("v"))
            agents = try c.decode([Agent].self, forKey: AnyKey("agents"))
            hidden = try c.decode(Int.self, forKey: AnyKey("hidden"))
        }
    }
}

/// The intents Swift sends to Rust through the callback (Fixtures/bridge/intent-*-v1.json).
/// Swift only reports what the user did; Rust decides what happens (AGENTS.md rule 3).
public enum CorralIntentWire {
    public static let wireVersion = 1

    private struct Envelope: Encodable {
        let v: Int
        let intent: Body
    }

    private struct Body: Encodable {
        let kind: String
        let id: String?
        let ids: [String]?
    }

    public static func encode(_ intent: CorralIntent) -> Data? {
        let body: Body
        switch intent {
        case .openAgent(let id): body = Body(kind: "openAgent", id: id, ids: nil)
        case .deploy(let ids): body = Body(kind: "deploy", id: nil, ids: ids)
        case .summonPromptBar: body = Body(kind: "summonPromptBar", id: nil, ids: nil)
        }
        // Encoding plain strings can't fail in practice; if it ever does, the tap is dropped
        // rather than sending Rust something malformed.
        return try? JSONEncoder().encode(Envelope(v: wireVersion, intent: body))
    }
}

struct AnyKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init(_ s: String) { stringValue = s }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}

private func rejectUnknownKeys(_ decoder: Decoder, allowed: Set<String>) throws {
    let c = try decoder.container(keyedBy: AnyKey.self)
    if let extra = c.allKeys.first(where: { !allowed.contains($0.stringValue) }) {
        throw DecodingError.dataCorruptedError(forKey: extra, in: c, debugDescription: "unknown key")
    }
}
