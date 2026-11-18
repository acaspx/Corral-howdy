// CorralRunCard.swift
// A drop's run card as Rust sends it on the roster (SK-6, Fixtures/bridge/roster-cards-v1.json):
// what the hover card and the result card show. Rust's core computed every field (elapsed time,
// spend, the cleaned answer, the buttons); Swift only draws it (AGENTS.md rule 3).
//
// Everything from Rust is untrusted (AGENTS.md, rules learned 2026-11-07), and answers are agent
// text (security model T20), so text is cleaned and capped again here: control and format
// characters (bidi included) go, except line breaks and the zero-width joiner that emoji need.
//
// A waiting checkpoint (M2) brings its approval box: the core's title and the exact detail, with
// the action class and risk. A checkpoint with a bad ID or a risk this build doesn't know is
// dropped, never guessed at, and with it the card's Approve and Deny.
//
// The checkpoint can carry its call's arguments as fields (TP-CHK-15), for the edit sheet. The
// core decides which can change; Swift shows anything odd read-only (hidden characters, a key too
// long to match, a kind it doesn't know) and drops a list with repeated keys or too many fields.

import Foundation

public struct CorralRunCard: Equatable, Sendable {
    public enum Status: String, Equatable, Sendable {
        case queued, working, needsYou, done, error, stopped
    }

    public enum Action: String, Equatable, Sendable {
        case stop, sendBack, newTask, retry, approve, deny
    }

    /// One argument of a waiting call, for the edit sheet. The core decides which can change.
    public struct Field: Equatable, Hashable, Sendable {
        public enum Kind: String, Equatable, Hashable, Sendable { case text, number, fixed }

        public static let maxFields = 20
        public static let maxKeyLength = 40
        public static let maxLabelLength = 60
        public static let maxEditableLength = 8_192
        public static let maxFixedLength = 200

        /// The argument's key, exactly: an edit names it.
        public var key: String
        public var label: String
        public var kind: Kind
        /// Exact for text and numbers; cleaned and cut for read-only ones.
        public var value: String

        public init(key: String, label: String, kind: Kind, value: String) {
            self.key = key
            self.label = label
            self.kind = kind
            self.value = value
        }

        /// A field from the wire. Editable only if the key and value come through exactly.
        init(_ wire: Wire) {
            key = wire.key
            label = CorralRunCard.clean(wire.label, max: Self.maxLabelLength).text
            let cleanKey = CorralRunCard.clean(wire.key, max: Self.maxKeyLength)
            let cleanValue = CorralRunCard.clean(wire.value, max: Self.maxEditableLength)
            let exact = cleanKey.text == wire.key && !cleanKey.cut && !wire.key.contains("\n")
                && cleanValue.text == wire.value && !cleanValue.cut
            if let kind = Kind(rawValue: wire.kind), kind != .fixed, exact {
                self.kind = kind
                self.value = wire.value
            } else {
                kind = .fixed
                self.value = CorralRunCard.clean(wire.value, max: Self.maxFixedLength).text
            }
        }

        struct Wire: Decodable {
            let key: String
            let label: String
            let kind: String
            let value: String

            init(from decoder: Decoder) throws {
                try rejectUnknownKeys(decoder, allowed: ["key", "label", "kind", "value"])
                let c = try decoder.container(keyedBy: AnyKey.self)
                key = try c.decode(String.self, forKey: AnyKey("key"))
                label = try c.decode(String.self, forKey: AnyKey("label"))
                kind = try c.decode(String.self, forKey: AnyKey("kind"))
                value = try c.decode(String.self, forKey: AnyKey("value"))
            }
        }
    }

    /// A checkpoint waiting on the user, from the core's `checkpoint.opened` only.
    public struct Checkpoint: Equatable, Sendable {
        public enum Risk: String, Equatable, Sendable { case normal, high, critical }

        public static let maxTitleLength = 120
        public static let maxDetailLength = 2_000
        public static let maxClassLength = 32

        /// The single-use ID an answer carries.
        public var id: String
        /// The action class, as the policy names it (`comms.send`).
        public var actionClass: String
        public var risk: Risk
        public var title: String
        /// The exact detail: tool, destination, arguments.
        public var detail: String
        /// Whether the core or the bridge cut the detail to fit.
        public var detailTruncated: Bool
        /// The call's arguments, for the edit sheet. Empty when the card can't edit them.
        public var fields: [Field]

        public init(id: String, actionClass: String, risk: Risk, title: String, detail: String,
                    detailTruncated: Bool = false, fields: [Field] = []) {
            self.id = id
            self.actionClass = actionClass
            self.risk = risk
            self.title = title
            self.detail = detail
            self.detailTruncated = detailTruncated
            self.fields = fields
        }

        /// A checkpoint from the wire, or nil for a bad ID or an unknown risk.
        init?(_ wire: Wire) {
            guard CorralRoster.isValidID(wire.id), let risk = Risk(rawValue: wire.risk) else { return nil }
            id = wire.id
            self.risk = risk
            actionClass = CorralRunCard.clean(wire.actionClass, max: Self.maxClassLength).text
            title = CorralRunCard.clean(wire.title, max: Self.maxTitleLength).text
            let detail = CorralRunCard.clean(wire.detail, max: Self.maxDetailLength)
            self.detail = detail.text
            detailTruncated = wire.detailTruncated || detail.cut
            let fields = (wire.fields ?? []).map(Field.init)
            let unique = Set(fields.map(\.key)).count == fields.count
            self.fields = unique && fields.count <= Field.maxFields ? fields : []
        }

        struct Wire: Decodable {
            let id: String
            /// `class` on the wire.
            let actionClass: String
            let risk: String
            let title: String
            let detail: String
            let detailTruncated: Bool
            let fields: [Field.Wire]?

            init(from decoder: Decoder) throws {
                try rejectUnknownKeys(decoder, allowed: ["id", "class", "risk", "title", "detail", "detailTruncated", "fields"])
                let c = try decoder.container(keyedBy: AnyKey.self)
                id = try c.decode(String.self, forKey: AnyKey("id"))
                actionClass = try c.decode(String.self, forKey: AnyKey("class"))
                risk = try c.decode(String.self, forKey: AnyKey("risk"))
                title = try c.decode(String.self, forKey: AnyKey("title"))
                detail = try c.decode(String.self, forKey: AnyKey("detail"))
                detailTruncated = try c.decode(Bool.self, forKey: AnyKey("detailTruncated"))
                fields = try c.decodeIfPresent([Field.Wire].self, forKey: AnyKey("fields"))
            }
        }
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
    /// The checkpoint waiting on the user, if any.
    public var checkpoint: Checkpoint?

    /// A card as the core would send it, for previews and the playground.
    public init(status: Status, prompt: String, elapsedMs: Int, spend: String,
                answer: String? = nil, error: String? = nil, actions: [Action],
                checkpoint: Checkpoint? = nil) {
        self.status = status
        self.prompt = prompt
        promptTruncated = false
        self.elapsedMs = elapsedMs
        self.spend = spend
        self.answer = answer
        answerTruncated = false
        self.error = error
        self.actions = actions
        self.checkpoint = checkpoint
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
        let checkpoint = wire.checkpoint.flatMap { Checkpoint($0) }
        self.checkpoint = checkpoint
        var actions: [Action] = []
        for raw in wire.actions {
            guard let action = Action(rawValue: raw), !actions.contains(action) else { continue }
            // Approve and Deny answer a checkpoint; without one they'd answer nothing.
            if checkpoint == nil, action == .approve || action == .deny { continue }
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
        let checkpoint: Checkpoint.Wire?

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: [
                "id", "status", "prompt", "promptTruncated", "elapsedMs", "spend", "answer",
                "answerTruncated", "error", "actions", "checkpoint",
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
            checkpoint = try c.decodeIfPresent(Checkpoint.Wire.self, forKey: AnyKey("checkpoint"))
        }
    }
}
