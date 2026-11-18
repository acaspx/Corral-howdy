// CorralCardModel.swift
// What a drop's card shows, from the core's run card (docs/07 §8.2, TP-HOVER-02).
//
// The core decides the run's state and its buttons (`CorralRunCard.actions`); this only words
// them. "New task" isn't a button: it's the task field at the bottom of the card, shown when the
// agent is in the corral, done or stopped. Pure, so every state is testable without a window.
//
// While a checkpoint waits, the card carries its approval box (`Approval`): only the core's
// fields, with the presentation rules from docs/07 §8.2. High-risk Approve needs a pinned card and
// the whole detail shown; critical has the same rules, then asks Rust for Touch ID (TP-CHK-18). The policy still
// checks every gesture in Rust; these rules only keep Approve from being pressed too easily.
//
// Edit… (TP-CHK-16) opens the call's arguments as fields, under the same rules as Approve. The
// sheet sends back only the fields the user changed; Rust merges them into the agent's arguments
// and checks them, so a read-only value can't change from here.

import Foundation

public struct CorralCardModel: Equatable, Sendable {
    public enum Button: Equatable, Hashable, Sendable {
        case stop, sendBack, retry
        /// A linked drop's only button: opens the app it lives in (ADR-005).
        case open(String)
        /// Approve the waiting checkpoint. `touchID`: it's critical, so it needs Touch ID.
        case approve(checkpoint: String, touchID: Bool)
        /// Deny the waiting checkpoint.
        case deny(checkpoint: String)
        /// Approve it for the rest of the run (in the approval box, not the button row).
        case allowForRun(checkpoint: String)
        /// Approve the edited call, from the edit sheet: only the fields that changed.
        case approveEdited(checkpoint: String, changes: [String: String])

        public var title: String {
            switch self {
            case .stop: "Stop"
            case .sendBack: "Send back"
            case .retry: "Retry"
            case .open(let app): "Open \(app)"
            case .approve(_, let touchID): touchID ? "Approve with Touch ID" : "Approve"
            case .deny: "Deny"
            case .allowForRun: "Allow for this run"
            case .approveEdited: "Approve edited"
            }
        }
    }

    /// The approval box: what's waiting, from the core's checkpoint only (docs/07 §8.2).
    public struct Approval: Equatable, Sendable {
        /// A detail longer than this, in characters or lines, shows collapsed until expanded.
        public static let collapsedCharacters = 240
        public static let collapsedLines = 4

        public var checkpointID: String
        public var title: String
        public var detail: String
        /// The core or the bridge cut the detail; the full arguments are in the trace.
        public var detailCut: Bool
        public var classLabel: String
        public var risk: CorralRunCard.Checkpoint.Risk
        public var riskLabel: String
        /// The call's arguments, for the edit sheet.
        public var fields: [CorralRunCard.Field]

        init(_ checkpoint: CorralRunCard.Checkpoint) {
            checkpointID = checkpoint.id
            title = checkpoint.title
            detail = checkpoint.detail
            detailCut = checkpoint.detailTruncated
            classLabel = checkpoint.actionClass
            risk = checkpoint.risk
            riskLabel = switch checkpoint.risk {
            case .normal: "Normal risk"
            case .high: "High risk"
            case .critical: "Critical"
            }
            fields = checkpoint.fields
        }

        /// Whether Edit… is offered: at least one field can change.
        public var editable: Bool { fields.contains { $0.kind != .fixed } }

        /// What the sheet sends: the editable fields whose text differs from the agent's.
        public func changes(_ draft: [String: String]) -> [String: String] {
            var changed: [String: String] = [:]
            for field in fields where field.kind != .fixed {
                if let typed = draft[field.key], typed != field.value { changed[field.key] = typed }
            }
            return changed
        }

        /// Why "Approve edited" can't be pressed yet, or nil when it can. Approve's rules first:
        /// the sheet shows every field, so only pinning and Touch ID still apply.
        public func editHint(_ draft: [String: String], pinned: Bool) -> String? {
            if risk == .critical { return "A critical action can't be edited yet. Approve it with Touch ID, or deny." }
            if let hint = approveHint(pinned: pinned, expanded: true) { return hint }
            let changed = changes(draft)
            if changed.isEmpty { return "Change a field to approve the edited action." }
            for field in fields where field.kind == .number {
                if let typed = changed[field.key], !Self.isNumber(typed) { return "\(field.label) needs a number." }
            }
            return nil
        }

        /// The button the sheet sends, or nil when nothing changed.
        public func approveEdited(_ draft: [String: String]) -> Button? {
            let changed = changes(draft)
            return changed.isEmpty ? nil : .approveEdited(checkpoint: checkpointID, changes: changed)
        }

        /// A JSON number, finite: what Rust will take. Rust checks again.
        static func isNumber(_ text: String) -> Bool {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            let pattern = "^-?(0|[1-9][0-9]*)(\\.[0-9]+)?([eE][+-]?[0-9]+)?$"
            guard trimmed.range(of: pattern, options: .regularExpression) != nil else { return false }
            return Double(trimmed)?.isFinite == true
        }

        /// Whether the detail is too long to show whole at first.
        public var needsExpanding: Bool {
            let lines = detail.split(separator: "\n", omittingEmptySubsequences: false)
            return detail.count > Self.collapsedCharacters || lines.count > Self.collapsedLines
        }

        /// Whether Approve can be pressed now.
        public func canApprove(pinned: Bool, expanded: Bool) -> Bool {
            approveHint(pinned: pinned, expanded: expanded) == nil
        }

        /// "Allow for this run" under the buttons, or nil where it isn't offered: a critical
        /// action needs Touch ID every time, and a budget limit is answered once. Rust pins a
        /// high-risk grant to the destination, so the card says so.
        public var forRunTitle: String? {
            guard classLabel != "budget.threshold" else { return nil }
            return switch risk {
            case .normal: "Allow for this run"
            case .high: "Allow for this run (this destination only)"
            case .critical: nil
            }
        }

        /// Why Approve can't be pressed yet, or nil when it can.
        public func approveHint(pinned: Bool, expanded: Bool) -> String? {
            switch risk {
            case .normal:
                return nil
            case .high, .critical:
                if !pinned { return "Click the drop to pin this card, then approve." }
                if needsExpanding, !expanded { return "Show the whole action before approving." }
                return nil
            }
        }
    }

    /// Small caps above the headline: the agent's name.
    public var label: String
    /// The task, or "Ready for a task".
    public var headline: String
    /// Where the run stands, with elapsed time and spend from the core.
    public var status: String
    /// The answer (agent text: already cleaned and capped by the core and the bridge).
    public var answer: String?
    public var answerTruncated: Bool
    /// Why the run failed.
    public var error: String?
    /// The approval box, while a checkpoint waits.
    public var approval: Approval?
    /// The buttons, in the core's order. The first is the primary one.
    public var buttons: [Button]
    /// The task field's placeholder, or nil when the card has no field.
    public var fieldPlaceholder: String?

    /// `linkedApp`: for a linked app's drop, the app it opens in. Corral can't give it tasks, so
    /// its card says so and offers only Open (ADR-005, docs/07 §8.3).
    public init(agentName: String, card: CorralRunCard?, linkedApp: String? = nil) {
        if let linkedApp {
            label = "\(agentName.uppercased()) · LINKED APP"
            headline = "Works in \(linkedApp)"
            status = "Not managed by Corral"
            answer = nil
            answerTruncated = false
            error = nil
            approval = nil
            buttons = [.open(linkedApp)]
            fieldPlaceholder = nil
            return
        }
        label = agentName.uppercased()
        guard let card else {
            headline = "Ready for a task"
            status = "In the corral"
            answer = nil
            answerTruncated = false
            error = nil
            approval = nil
            buttons = []
            fieldPlaceholder = "Give \(agentName) a task"
            return
        }
        headline = card.prompt
        let time = Self.duration(ms: card.elapsedMs)
        status = switch card.status {
        case .queued: "Waiting to start"
        case .working: "Working · \(time) · \(card.spend)"
        case .needsYou: "Needs your approval · \(time)"
        case .done: "Done in \(time) · \(card.spend)"
        case .error: "Didn't finish · \(time)"
        case .stopped: "Stopped after \(time)"
        }
        answer = card.answer
        answerTruncated = card.answerTruncated
        error = card.error
        let approval = card.checkpoint.map(Approval.init)
        self.approval = approval
        buttons = card.actions.compactMap { action -> Button? in
            switch action {
            case .stop: .stop
            case .sendBack: .sendBack
            case .retry: .retry
            case .newTask: nil
            case .approve: approval.map { Button.approve(checkpoint: $0.checkpointID, touchID: $0.risk == .critical) }
            case .deny: approval.map { Button.deny(checkpoint: $0.checkpointID) }
            }
        }
        fieldPlaceholder = if approval != nil, card.actions.contains(.deny) {
            "Deny with a note for \(agentName)"
        } else if card.actions.contains(.newTask) {
            card.status == .done ? "Ask a follow-up" : "Give \(agentName) a new task"
        } else {
            nil
        }
    }

    /// The intent a button sends.
    public static func intent(for button: Button, agentID: String) -> CorralIntent {
        switch button {
        case .stop: .stop(id: agentID)
        case .sendBack: .sendBack(id: agentID)
        case .retry: .retry(id: agentID)
        case .open: .openLinked(id: agentID)
        // A critical Approve asks Rust for Touch ID; Rust has macOS prompt and decides on the result.
        case .approve(let checkpoint, let touchID):
            touchID ? .approveWithTouchId(id: agentID, checkpoint: checkpoint)
                    : .approveCheckpoint(id: agentID, checkpoint: checkpoint, forRun: false)
        case .deny(let checkpoint): .denyCheckpoint(id: agentID, checkpoint: checkpoint, note: nil)
        case .allowForRun(let checkpoint): .approveCheckpoint(id: agentID, checkpoint: checkpoint, forRun: true)
        case .approveEdited(let checkpoint, let changes):
            .editCheckpoint(id: agentID, checkpoint: checkpoint, changes: changes)
        }
    }

    /// What Return in the card's field sends: Deny with the text as its note while a checkpoint
    /// waits, otherwise the text as a task. Nil for empty text or a card with no field.
    public func fieldIntent(_ text: String, agentID: String) -> CorralIntent? {
        guard fieldPlaceholder != nil else { return nil }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let approval, buttons.contains(.deny(checkpoint: approval.checkpointID)) {
            return .denyCheckpoint(id: agentID, checkpoint: approval.checkpointID, note: trimmed)
        }
        return Self.task(trimmed, agentID: agentID)
    }

    /// The intent the task field sends, or nil for an empty task.
    public static func task(_ text: String, agentID: String) -> CorralIntent? {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return prompt.isEmpty ? nil : .deployTask(id: agentID, prompt: prompt)
    }

    /// "5s", "1m 16s", "1h 2m".
    public static func duration(ms: Int) -> String {
        let s = max(ms, 0) / 1000
        if s < 60 { return "\(s)s" }
        if s < 3600 { return "\(s / 60)m \(s % 60)s" }
        return "\(s / 3600)h \((s % 3600) / 60)m"
    }
}

extension CorralCardModel.Button {
    /// Whether the button can be the card's primary (filled) button when it comes first: Stop
    /// never is, so it can't be pressed by habit.
    public var isPrimary: Bool { self != .stop }
}

/// Buttons ignore input for 300 ms after a card appears, so a card opening under the pointer
/// can't be clicked through by accident (docs/07 §8.2, TP-HOVER-05).
public struct CorralInputGuard: Equatable, Sendable {
    public static let delay: TimeInterval = 0.3
    public let shownAt: TimeInterval

    public init(shownAt: TimeInterval) { self.shownAt = shownAt }

    public func accepts(at now: TimeInterval) -> Bool { now - shownAt >= Self.delay }
}
