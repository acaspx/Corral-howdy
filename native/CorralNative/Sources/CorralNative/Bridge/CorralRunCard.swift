// CorralRunCard.swift
// A drop's run card as Rust sends it on the roster (SK-6, Fixtures/bridge/roster-cards-v1.json):
// what the hover card and the result card show. Rust's core computed every field (elapsed time,
// spend, the cleaned answer, the buttons); Swift only draws it (AGENTS.md rule 3).
//
// Everything from Rust is untrusted (AGENTS.md, rules learned 2026-11-07), and answers are agent
// text (security model T20), so text is cleaned and capped again here: control and format
// characters (bidi included) go, except line breaks and the zero-width joiner that emoji need.

import Foundation

public struct CorralRunCard: Equatable, Sendable {
    public enum Status: String, Equatable, Sendable {
        case queued, working, needsYou, done, error, stopped
    }

    public enum Action: String, Equatable, Sendable {
        case stop, sendBack, newTask, retry
    }

    public static let maxPromptLength = 280
    public static let maxAnswerLength = 4_000
    public static let maxErrorLength = 280
    public static let maxSpendLength = 16
    /// A month: anything longer is a bad clock, not a long run.
    public static let maxElapsedMs = 31 * 24 * 3_600_000

    public var status: Status
    public var prompt: String
    public var promptTruncated: Bool
    public var elapsedMs: Int
    public var spend: String
    public var answer: String?
    public var answerTruncated: Bool
    public var error: String?
    /// The buttons, in order. Unknown ones are dropped, never guessed at.
    public var actions: [Action]

    /// A card as the core would send it, for previews and the playground.
    public init(status: Status, prompt: String, elapsedMs: Int, spend: String,
                answer: String? = nil, error: String? = nil, actions: [Action]) {
        self.status = status
        self.prompt = prompt
        promptTruncated = false
        self.elapsedMs = elapsedMs
        self.spend = spend
        self.answer = answer
        answerTruncated = false
        self.error = error
        self.actions = actions
    }

    /// A card from the wire, or nil for a status this build doesn't know.
    init?(_ wire: Wire) {
        guard let status = Status(rawValue: wire.status) else { return nil }
        self.status = status
        let prompt = Self.clean(wire.prompt, max: Self.maxPromptLength)
        self.prompt = prompt.text
        promptTruncated = wire.promptTruncated || prompt.cut
        elapsedMs = min(max(wire.elapsedMs, 0), Self.maxElapsedMs)
        spend = Self.clean(wire.spend, max: Self.maxSpendLength).text
        let answer = wire.answer.map { Self.clean($0, max: Self.maxAnswerLength) }
        self.answer = answer?.text
        answerTruncated = wire.answerTruncated || (answer?.cut ?? false)
        error = wire.error.map { Self.clean($0, max: Self.maxErrorLength).text }
        var actions: [Action] = []
        for raw in wire.actions {
            guard let action = Action(rawValue: raw), !actions.contains(action) else { continue }
            actions.append(action)
        }
        self.actions = actions
    }

    /// Removes control and format characters except line breaks and the zero-width joiner, and
    /// keeps at most `max` characters. `cut` says whether anything was cut off.
    static func clean(_ s: String, max: Int) -> (text: String, cut: Bool) {
        var scalars = String.UnicodeScalarView()
        scalars.append(contentsOf: s.unicodeScalars.filter {
            $0 == "\n" || $0 == "\u{200D}" || !CharacterSet.controlCharacters.contains($0)
        })
        let all = String(scalars)
        return (String(all.prefix(max)), all.count > max)
    }

    // MARK: Wire format. Unknown keys are rejected, like Rust's `deny_unknown_fields`.

    struct Wire: Decodable {
        let id: String
        let status: String
        let prompt: String
        let promptTruncated: Bool
        let elapsedMs: Int
        let spend: String
        let answer: String?
        let answerTruncated: Bool
        let error: String?
        let actions: [String]

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: [
                "id", "status", "prompt", "promptTruncated", "elapsedMs", "spend", "answer",
                "answerTruncated", "error", "actions",
            ])
            let c = try decoder.container(keyedBy: AnyKey.self)
            id = try c.decode(String.self, forKey: AnyKey("id"))
            status = try c.decode(String.self, forKey: AnyKey("status"))
            prompt = try c.decode(String.self, forKey: AnyKey("prompt"))
            promptTruncated = try c.decode(Bool.self, forKey: AnyKey("promptTruncated"))
            elapsedMs = try c.decode(Int.self, forKey: AnyKey("elapsedMs"))
            spend = try c.decode(String.self, forKey: AnyKey("spend"))
            answer = try c.decodeIfPresent(String.self, forKey: AnyKey("answer"))
            answerTruncated = try c.decode(Bool.self, forKey: AnyKey("answerTruncated"))
            error = try c.decodeIfPresent(String.self, forKey: AnyKey("error"))
            actions = try c.decode([String].self, forKey: AnyKey("actions"))
        }
    }
}
