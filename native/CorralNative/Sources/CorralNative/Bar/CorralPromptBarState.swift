// CorralPromptBarState.swift
// What the prompt bar does with keys and with Rust's answers (docs/07 §6.1, TP-BAR-01, 04..06).
//
// - Opening asks Rust for the view. Dragging a drop up opens it with that agent named, before any
//   draft. Closing without sending keeps the text until it's sent or cleared (⌘⌫).
// - ↑ and ↓ move through the @ suggestions; Return or Tab takes the highlighted one; Esc closes
//   the list, then the bar. With no list, Return sends and ⌘Return is Deploy all.
// - A send waits for Rust's answer: the field doesn't change meanwhile (so the answer is never
//   dropped as stale). Sent closes the bar; a note keeps it open with the text cleared; a refusal
//   keeps the text.
//
// Pure, so tests need no window. Who a prompt goes to is Rust's (AGENTS.md rule 3).
// `CorralPromptBarController` drives it.

import Foundation

public struct CorralPromptBarState: Equatable, Sendable {
    public enum Key: Equatable, Sendable {
        case up, down, enter, commandEnter, tab, escape
        /// ⌘⌫: clear the field.
        case clear
    }

    /// What the controller does next.
    public enum Action: Equatable, Sendable {
        /// Nothing more: the key was used.
        case none
        /// The key isn't the bar's: let the field have it.
        case pass
        /// The text or cursor changed: tell Rust.
        case edited
        /// Send the text: Return (`all` false), or ⌘Return and Deploy all (`all` true).
        case send(all: Bool)
        /// Close the bar, keeping the text.
        case close
    }

    /// A send's answer, shown in the error line until the next edit.
    public enum Message: Equatable, Sendable {
        case note(String)
        case refused(String)
    }

    public private(set) var isOpen = false
    public private(set) var text = ""
    /// Where the cursor is, in UTF-16, as the field gives it.
    public private(set) var cursor = 0
    /// The highlighted suggestion.
    public private(set) var highlighted = 0
    /// Esc closed the suggestion list; typing shows it again.
    public private(set) var listClosed = false
    /// A send is waiting for Rust's answer.
    public private(set) var sending = false
    public private(set) var message: Message?

    public init() {}

    /// Whether the suggestion list shows, for `count` rows from Rust.
    public func listShown(_ count: Int) -> Bool { count > 0 && !listClosed }

    /// Opens the bar; with `name`, that agent is named first (a drop dragged up).
    public mutating func open(naming name: String?) -> Action {
        isOpen = true
        sending = false
        message = nil
        if let name {
            let named = "@\(name) "
            if !text.hasPrefix(named) {
                text = named + text
                cursor = named.utf16.count
                textChanged()
            }
        }
        return .edited
    }

    /// The summon shortcut: opens the bar, or closes it if it's open (TP-BAR-09).
    public mutating func summoned() -> Action {
        guard isOpen else { return open(naming: nil) }
        close()
        return .close
    }

    /// Closes the bar, keeping the text (Esc, the hotkey, a click elsewhere).
    public mutating func close() {
        isOpen = false
        sending = false
    }

    /// The field changed.
    public mutating func edit(text newText: String, cursor newCursor: Int) -> Action {
        guard !sending else { return .none }
        let newCursor = max(0, min(newCursor, newText.utf16.count))
        guard newText != text || newCursor != cursor else { return .none }
        if newText != text {
            text = newText
            textChanged()
        }
        cursor = newCursor
        return .edited
    }

    /// A key in the field, with the rows Rust sent for it.
    public mutating func key(_ key: Key, suggestions: [CorralPromptBarView.Suggestion]) -> Action {
        let list = listShown(suggestions.count)
        switch key {
        case .up, .down:
            guard list else { return .pass }
            let at = min(highlighted, suggestions.count - 1)
            highlighted = key == .up ? max(0, at - 1) : min(suggestions.count - 1, at + 1)
            return .none
        case .enter, .tab:
            if list { return take(suggestions[min(highlighted, suggestions.count - 1)]) }
            guard key == .enter else { return .pass }
            return startSending(all: false)
        case .commandEnter:
            return startSending(all: true)
        case .escape:
            if list {
                listClosed = true
                return .none
            }
            close()
            return .close
        case .clear:
            guard !sending else { return .none }
            text = ""
            cursor = 0
            textChanged()
            return .edited
        }
    }

    /// A click on suggestion row `index`.
    public mutating func choose(_ index: Int, suggestions: [CorralPromptBarView.Suggestion]) -> Action {
        guard listShown(suggestions.count), suggestions.indices.contains(index) else { return .none }
        return take(suggestions[index])
    }

    /// Rust's answer arrived. `nil` is an edit's answer: nothing to do here.
    public mutating func answered(_ outcome: CorralPromptBarOutcome?) {
        guard let outcome else { return }
        sending = false
        switch outcome {
        case .sent:
            text = ""
            cursor = 0
            textChanged()
            isOpen = false
        case .note(let note):
            text = ""
            cursor = 0
            textChanged()
            message = .note(note)
        case .refused(let why):
            message = .refused(why)
        }
    }

    private mutating func startSending(all: Bool) -> Action {
        guard !sending else { return .none }
        sending = true
        return .send(all: all)
    }

    /// Takes a suggestion: its completion replaces the text before the cursor.
    private mutating func take(_ row: CorralPromptBarView.Suggestion) -> Action {
        guard !sending else { return .none }
        let split = String.Index(utf16Offset: min(cursor, text.utf16.count), in: text)
        text = row.completion + text[split...]
        cursor = row.completion.utf16.count
        textChanged()
        return .edited
    }

    private mutating func textChanged() {
        highlighted = 0
        listClosed = false
        message = nil
    }
}
