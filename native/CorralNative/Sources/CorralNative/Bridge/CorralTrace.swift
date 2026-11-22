// CorralTrace.swift
// The trace tree above the corral, as the roster carries it (docs/07 §8.3, TP-TRACE-02, 06,
// Fixtures/bridge/roster-trace-v1.json): at most 3 columns, each with the prompt, the current
// subtask and up to 2 peeking below, plus how many more are running. The core chose and ordered
// them; Swift draws them. What arrives is still untrusted: bounded, IDs checked and deduped,
// unknown statuses and impossible subtasks dropped, never trapped on.

import Foundation

public struct CorralTrace: Equatable, Sendable {
    public static let maxColumns = 3
    public static let maxPeek = 2
    public static let maxMore = 9_999
    public static let maxTitleLength = 80
    public static let maxSubtasks = 100

    public enum Status: String, Equatable, Sendable {
        case queued, working, needsYou
    }

    /// "Subtask 2 of 5", and its title.
    public struct Subtask: Equatable, Sendable {
        public var step: Int
        public var total: Int
        public var title: String
        public init(step: Int, total: Int, title: String) {
            self.step = step; self.total = total; self.title = title
        }
    }

    public struct Column: Equatable, Sendable {
        /// The agent; the column's wire ends at its drop, or the hidden bead.
        public var id: String
        public var status: Status
        public var prompt: String
        public var promptTruncated: Bool
        public var subtask: Subtask?
        /// What's left after the current subtask, at most 2.
        public var next: [String]
    }

    /// At most 3, in the core's order.
    public var columns: [Column]
    /// Running beyond those.
    public var more: Int

    public static let empty = CorralTrace(columns: [], more: 0)

    init(columns: [Column], more: Int) {
        self.columns = columns
        self.more = more
    }

    init(_ wire: Wire) {
        var seen = Set<String>()
        var columns: [Column] = []
        var overflow = 0
        for column in wire.columns {
            guard CorralRoster.isValidID(column.id), let status = Status(rawValue: column.status),
                  seen.insert(column.id).inserted else { continue }
            guard columns.count < Self.maxColumns else { overflow += 1; continue }
            let prompt = CorralRunCard.clean(column.prompt, max: CorralRunCard.maxPromptLength)
            let subtask = column.subtask.flatMap { s -> Subtask? in
                guard (1...Self.maxSubtasks).contains(s.total), (1...s.total).contains(s.step) else { return nil }
                return Subtask(step: s.step, total: s.total,
                               title: CorralRunCard.clean(s.title, max: Self.maxTitleLength).text)
            }
            columns.append(Column(
                id: column.id, status: status, prompt: prompt.text,
                promptTruncated: column.promptTruncated || prompt.cut, subtask: subtask,
                next: column.next.prefix(Self.maxPeek).map { CorralRunCard.clean($0, max: Self.maxTitleLength).text }))
        }
        self.columns = columns
        more = min(max(wire.more, 0) + overflow, Self.maxMore)
    }

    // MARK: Wire format. Unknown keys are rejected, like Rust's `deny_unknown_fields`.

    struct Wire: Decodable {
        let columns: [ColumnWire]
        let more: Int

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["columns", "more"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            columns = try c.decode([ColumnWire].self, forKey: AnyKey("columns"))
            more = try c.decode(Int.self, forKey: AnyKey("more"))
        }
    }

    struct ColumnWire: Decodable {
        let id: String
        let status: String
        let prompt: String
        let promptTruncated: Bool
        let subtask: SubtaskWire?
        let next: [String]

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["id", "status", "prompt", "promptTruncated", "subtask", "next"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            id = try c.decode(String.self, forKey: AnyKey("id"))
            status = try c.decode(String.self, forKey: AnyKey("status"))
            prompt = try c.decode(String.self, forKey: AnyKey("prompt"))
            promptTruncated = try c.decode(Bool.self, forKey: AnyKey("promptTruncated"))
            subtask = try c.decodeIfPresent(SubtaskWire.self, forKey: AnyKey("subtask"))
            next = try c.decode([String].self, forKey: AnyKey("next"))
        }
    }

    struct SubtaskWire: Decodable {
        let step: Int
        let total: Int
        let title: String

        init(from decoder: Decoder) throws {
            try rejectUnknownKeys(decoder, allowed: ["step", "total", "title"])
            let c = try decoder.container(keyedBy: AnyKey.self)
            step = try c.decode(Int.self, forKey: AnyKey("step"))
            total = try c.decode(Int.self, forKey: AnyKey("total"))
            title = try c.decode(String.self, forKey: AnyKey("title"))
        }
    }
}
