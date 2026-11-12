// CorralCardModel.swift
// What a drop's card shows, from the core's run card (docs/07 §8.2, TP-HOVER-02).
//
// The core decides the run's state and its buttons (`CorralRunCard.actions`); this only words
// them. "New task" isn't a button: it's the task field at the bottom of the card, shown when the
// agent is in the corral, done or stopped. Pure, so every state is testable without a window.

import Foundation

public struct CorralCardModel: Equatable, Sendable {
    public enum Button: Equatable, Hashable, Sendable {
        case stop, sendBack, retry

        public var title: String {
            switch self {
            case .stop: "Stop"
            case .sendBack: "Send back"
            case .retry: "Retry"
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
    /// The buttons, in the core's order. The first is the primary one.
    public var buttons: [Button]
    /// The task field's placeholder, or nil when the card has no field.
    public var fieldPlaceholder: String?

    public init(agentName: String, card: CorralRunCard?) {
        label = agentName.uppercased()
        guard let card else {
            headline = "Ready for a task"
            status = "In the corral"
            answer = nil
            answerTruncated = false
            error = nil
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
        buttons = card.actions.compactMap { action -> Button? in
            switch action {
            case .stop: .stop
            case .sendBack: .sendBack
            case .retry: .retry
            case .newTask: nil
            }
        }
        fieldPlaceholder = if card.actions.contains(.newTask) {
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
        }
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
