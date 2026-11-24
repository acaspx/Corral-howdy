// CorralKeyboard.swift
// The corral from the keyboard (docs/07 §10, TP-A11Y-01, TP-A11Y-03).
//
// The path: summon the prompt bar, ↓ moves into the corral, ← and → move between drops in rank
// order (the visual clockwise order), Space or Return opens the focused drop's card pinned, Tab
// goes through its buttons, Esc closes it and puts focus back on the drop, and ↑ or Esc goes back
// to the bar. A usage reorder never moves focus: it follows the drop.
//
// Pure, so tests need no window. `CorralGlassView` reads the keys; the bridge acts on the moves.

import Foundation

public enum CorralKeyboard {
    public enum Key: Equatable, Sendable {
        case left, right, up, down, space, enter, escape
    }

    /// What a key does in the corral.
    public enum Move: Equatable, Sendable {
        /// Focus this drop.
        case focus(String)
        /// Open this drop's card, pinned, from the keyboard.
        case open(String)
        /// Back to the prompt bar.
        case toBar
        /// The key isn't the corral's.
        case none
    }

    /// The drop that takes focus when the corral does: the most used.
    public static func entry(ranked: [String]) -> String? { ranked.first }

    /// `focused`, if it's still in the corral: a reorder keeps focus on the same drop.
    public static func kept(_ focused: String?, ranked: [String]) -> String? {
        guard let focused, ranked.contains(focused) else { return nil }
        return focused
    }

    /// What `key` does with `focused` and the drops in rank order.
    public static func move(_ key: Key, from focused: String?, ranked: [String]) -> Move {
        switch key {
        case .up, .escape:
            return .toBar
        case .space, .enter:
            guard let id = kept(focused, ranked: ranked) else { return .none }
            return .open(id)
        case .left, .right, .down:
            guard let first = ranked.first else { return .none }
            guard let id = kept(focused, ranked: ranked), let at = ranked.firstIndex(of: id) else {
                return .focus(first)
            }
            let step = key == .left ? ranked.count - 1 : 1
            return .focus(ranked[(at + step) % ranked.count])
        }
    }

    /// VoiceOver reads higher priorities first, so the most used drop comes first.
    public static func voiceOverPriority(rank: Int) -> Double { -Double(rank) }
}

/// Asks the corral to take keyboard focus: on drop `id`, or on the most used if that's gone or
/// nil. Each request has a new `count`, so asking twice for the same drop still moves focus.
public struct CorralFocusRequest: Equatable, Sendable {
    public var id: String?
    public var count: Int

    public init(id: String? = nil, count: Int = 0) {
        self.id = id
        self.count = count
    }
}
