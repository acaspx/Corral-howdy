// CorralPromptBarWire.swift
// The prompt bar's wire (docs/07 §6.1, TP-BAR-08): Swift reports each edit and send
// (Fixtures/bridge/intent-prompt-bar-*-v1.json), and Rust answers with what the bar shows and how a
// send went (prompt-bar-v1.json, prompt-bar-note-v1.json) through `corral_native_prompt_bar`.
//
// Rust decides who a prompt goes to, the button, the error line and the suggestions (AGENTS.md
// rule 3). What it sends is still untrusted here: bounded, deduped, control characters stripped,
// and never trapped on. An answer that can't be read is rejected whole.

import Foundation

/// What the prompt bar shows, from Rust.
public struct CorralPromptBarView: Equatable, Sendable {
    public static let maxChips = 8
    public static let maxSuggestions = 8
    public static let maxNameLength = CorralRoster.maxNameLength
    public static let maxLabelLength = 40
    public static let maxPlaceholderLength = 300
    public static let maxErrorLength = 300
    /// A completion is the user's own text up to the name, plus the name: the core's prompt limit
    /// and a little.
    public static let maxCompletionLength = 9_000

    public struct Button: Equatable, Sendable {
        public var label: String
        public var enabled: Bool
        /// It's Deploy all (the button, or ⌘Return).
        public var all: Bool
        public init(label: String, enabled: Bool, all: Bool) {
            self.label = label; self.enabled = enabled; self.all = all
        }
    }

    public enum Status: String, Equatable, Sendable {
        case ready, working, needsYou
    }

    public struct Suggestion: Equatable, Sendable {
        public var id: String
        public var name: String
        /// Whether the corral shows it; hidden ones are marked "hidden".
        public var shown: Bool
        public var status: Status
        /// The text with the name completed: taking the row sets the field to this.
        public var completion: String
        public init(id: String, name: String, shown: Bool, status: Status, completion: String) {
            self.id = id; self.name = name; self.shown = shown; self.status = status; self.completion = completion
        }
    }

    /// Who it goes to: a chip per agent named, the one selected, or "3 selected".
    public var chips: [String]
    /// What Return does, for an empty field.
    public var placeholder: String
    public var button: Button
    /// The error line, in the core's words.
    public var error: String?
    /// The agents the name being typed could be, in the corral's order.
    public var suggestions: [Suggestion]
}

/// How a send went.
public enum CorralPromptBarOutcome: Equatable, Sendable {
    /// Every agent got it: the bar closes.
    case sent
    /// Some didn't: the bar stays open with this, its text cleared.
    case note(String)
    /// Nothing was sent: the bar stays open with this, its text kept.
    case refused(String)

    public static let maxTextLength = 600
}

/// Rust's answer to an edit or a send.
public struct CorralPromptBarSnapshot: Equatable, Sendable {
    public static let wireVersion = 1
    /// Largest answer accepted: 8 suggestions with full-length completions.
    public static let maxBytes = 128 * 1024

    /// The edit or send it answers.
    public var seq: UInt32
    public var view: CorralPromptBarView
    /// How a send went; nil for an edit.
    public var outcome: CorralPromptBarOutcome?

    public init(seq: UInt32, view: CorralPromptBarView, outcome: CorralPromptBarOutcome?) {
        self.seq = seq; self.view = view; self.outcome = outcome
    }

    public enum DecodeError: Error, Equatable, Sendable {
        case empty
        case tooLarge(Int)
        case malformed
        case unsupportedVersion(Int)
    }

    public static func decode(_ data: Data) -> Result<CorralPromptBarSnapshot, DecodeError> {
        guard !data.isEmpty else { return .failure(.empty) }
        guard data.count <= maxBytes else { return .failure(.tooLarge(data.count)) }
        let wire: Wire
        do { wire = try JSONDecoder().decode(Wire.self, from: data) } catch { return .failure(.malformed) }
        guard wire.v == wireVersion else { return .failure(.unsupportedVersion(wire.v)) }

        let clean = CorralRoster.sanitize
        var seen = Set<String>()
        var rows: [CorralPromptBarView.Suggestion] = []
        for row in wire.view.suggestions where rows.count < CorralPromptBarView.maxSuggestions {
            // IDs are identity: a bad or repeated one drops the row rather than being rewritten.
            guard CorralRoster.isValidID(row.id), seen.insert(row.id).inserted else { continue }
            rows.append(.init(
                id: row.id,
                name: clean(row.name, CorralPromptBarView.maxNameLength),
                shown: row.shown,
                status: row.status,
                // The user's own text: bounded, not rewritten, so taking a row never changes it.
                completion: String(row.completion.prefix(CorralPromptBarView.maxCompletionLength))))
        }
        let view = CorralPromptBarView(
            chips: wire.view.chips.prefix(CorralPromptBarView.maxChips)
                .map { clean($0, CorralPromptBarView.maxNameLength) },
            placeholder: clean(wire.view.placeholder, CorralPromptBarView.maxPlaceholderLength),
            button: .init(label: clean(wire.view.button.label, CorralPromptBarView.maxLabelLength),
                          enabled: wire.view.button.enabled, all: wire.view.button.all),
            error: wire.view.error.map { clean($0, CorralPromptBarView.maxErrorLength) },
            suggestions: rows)
        let outcome: CorralPromptBarOutcome?
        switch wire.outcome {
        case nil: outcome = nil
        case .sent?: outcome = .sent
        case .note(let text)?: outcome = .note(clean(text, CorralPromptBarOutcome.maxTextLength))
        case .refused(let text)?: outcome = .refused(clean(text, CorralPromptBarOutcome.maxTextLength))
        }
        return .success(CorralPromptBarSnapshot(seq: wire.seq, view: view, outcome: outcome))
    }

    // MARK: Wire format. Unknown keys, statuses and outcome kinds are rejected, like Rust's
    // `deny_unknown_fields`, so the two sides can't silently drift apart.

    private struct Wire: Decodable {
        let v: Int
        let seq: UInt32
        let view: View
        let outcome: Outcome?

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["v", "seq", "view", "outcome"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            v = try c.decode(Int.self, forKey: AnyKey("v"))
            seq = try c.decode(UInt32.self, forKey: AnyKey("seq"))
            view = try c.decode(View.self, forKey: AnyKey("view"))
            outcome = try c.decodeIfPresent(Outcome.self, forKey: AnyKey("outcome"))
        }

        struct View: Decodable {
            let chips: [String]
            let placeholder: String
            let button: Button
            let error: String?
            let suggestions: [Row]

            init(from decoder: Decoder) throws {
                try rejectUnknownKeys(decoder, allowed: ["chips", "placeholder", "button", "error", "suggestions"])
                let c = try decoder.container(keyedBy: AnyKey.self)
                chips = try c.decode([String].self, forKey: AnyKey("chips"))
                placeholder = try c.decode(String.self, forKey: AnyKey("placeholder"))
                button = try c.decode(Button.self, forKey: AnyKey("button"))
                error = try c.decodeIfPresent(String.self, forKey: AnyKey("error"))
                suggestions = try c.decode([Row].self, forKey: AnyKey("suggestions"))
            }
        }

        struct Button: Decodable {
            let label: String
            let enabled: Bool
            let all: Bool

            init(from decoder: Decoder) throws {
                try rejectUnknownKeys(decoder, allowed: ["label", "enabled", "all"])
                let c = try decoder.container(keyedBy: AnyKey.self)
                label = try c.decode(String.self, forKey: AnyKey("label"))
                enabled = try c.decode(Bool.self, forKey: AnyKey("enabled"))
                all = try c.decode(Bool.self, forKey: AnyKey("all"))
            }
        }

        struct Row: Decodable {
            let id: String
            let name: String
            let shown: Bool
            let status: CorralPromptBarView.Status
            let completion: String

            init(from decoder: Decoder) throws {
                try rejectUnknownKeys(decoder, allowed: ["id", "name", "shown", "status", "completion"])
                let c = try decoder.container(keyedBy: AnyKey.self)
                id = try c.decode(String.self, forKey: AnyKey("id"))
                name = try c.decode(String.self, forKey: AnyKey("name"))
                shown = try c.decode(Bool.self, forKey: AnyKey("shown"))
                let raw = try c.decode(String.self, forKey: AnyKey("status"))
                guard let status = CorralPromptBarView.Status(rawValue: raw) else {
                    throw DecodingError.dataCorruptedError(forKey: AnyKey("status"), in: c,
                                                           debugDescription: "unknown status")
                }
                self.status = status
                completion = try c.decode(String.self, forKey: AnyKey("completion"))
            }
        }

        enum Outcome: Decodable {
            case sent
            case note(String)
            case refused(String)

            init(from decoder: Decoder) throws {
                let c = try decoder.container(keyedBy: AnyKey.self)
                let kind = try c.decode(String.self, forKey: AnyKey("kind"))
                switch kind {
                case "sent":
                    try rejectUnknownKeys(decoder, allowed: ["kind"])
                    self = .sent
                case "note", "refused":
                    try rejectUnknownKeys(decoder, allowed: ["kind", "text"])
                    let text = try c.decode(String.self, forKey: AnyKey("text"))
                    self = kind == "note" ? .note(text) : .refused(text)
                default:
                    throw DecodingError.dataCorruptedError(forKey: AnyKey("kind"), in: c,
                                                           debugDescription: "unknown outcome")
                }
            }
        }
    }
}

/// The prompt bar's state: Rust's latest answer. Each edit and send gets the next number, and an
/// answer for any other number is dropped, so a slow answer never overwrites a newer one.
@MainActor
public final class CorralPromptBar: ObservableObject {
    @Published public private(set) var view: CorralPromptBarView?
    @Published public private(set) var outcome: CorralPromptBarOutcome?
    private var seq: UInt32 = 0

    public init() {}

    /// The intent for an edit: the text and the cursor (as AppKit gives it, in UTF-16).
    public func edited(text: String, utf16Cursor: Int, selection: [String]) -> CorralIntent {
        seq &+= 1
        return .promptBarEdited(seq: seq, text: text,
                                cursor: Self.utf8Cursor(in: text, utf16: utf16Cursor),
                                selection: Self.bounded(selection))
    }

    /// The intent for Return (`all` false) or Deploy all and ⌘Return (`all` true).
    public func send(text: String, selection: [String], all: Bool) -> CorralIntent {
        seq &+= 1
        return .promptBarSend(seq: seq, text: text, selection: Self.bounded(selection), all: all)
    }

    /// Takes Rust's answer if it's for the latest edit or send.
    @discardableResult
    public func receive(_ answer: CorralPromptBarSnapshot) -> Bool {
        guard answer.seq == seq else { return false }
        view = answer.view
        outcome = answer.outcome
        return true
    }

    /// The selection as Rust takes it: at most 8 valid IDs, each once. Pure, so any thread.
    nonisolated static func bounded(_ selection: [String]) -> [String] {
        var seen = Set<String>()
        return Array(selection.filter { CorralRoster.isValidID($0) && seen.insert($0).inserted }
            .prefix(CorralRoster.maxShown))
    }

    /// A UTF-16 offset as a UTF-8 one, clamped to the text and moved back to the start of the
    /// character it falls inside. Pure, so any thread.
    public nonisolated static func utf8Cursor(in text: String, utf16 offset: Int) -> Int {
        let target = max(0, min(offset, text.utf16.count))
        var utf16 = 0
        var utf8 = 0
        for scalar in text.unicodeScalars {
            let width = scalar.utf16.count
            if utf16 + width > target { break }
            utf16 += width
            utf8 += scalar.utf8.count
        }
        return utf8
    }
}
