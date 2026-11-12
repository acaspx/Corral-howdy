// CorralHoverState.swift
// When a drop's card opens, stays and closes (docs/07 §8.2, TP-HOVER-01).
//
// - Hovering a drop opens its card after 120 ms, without taking focus from the app in front.
// - The card stays while the pointer is on the drop or the card, and closes 260 ms after it leaves.
// - Clicking a drop pins its card: it stays until ×, Esc or a click elsewhere, and hovering other
//   drops doesn't swap it. Clicking another drop pins that one instead.
//
// Pure: time is passed in, so tests need no clock or window. `CorralCardController` drives it.

import Foundation

public struct CorralHoverState: Equatable, Sendable {
    public static let openDelay: TimeInterval = 0.12
    public static let closeDelay: TimeInterval = 0.26

    /// The drop whose card is showing.
    public private(set) var shown: String?
    /// Whether the card was clicked open.
    public private(set) var pinned = false

    private var overDrop: String?
    private var overCard = false
    private var opening: String?
    private var openAt: TimeInterval?
    private var closeAt: TimeInterval?

    public init() {}

    /// When `tick` next needs to run, if anything is waiting.
    public var deadline: TimeInterval? {
        switch (openAt, closeAt) {
        case let (open?, close?): min(open, close)
        case let (open?, nil): open
        case let (nil, close?): close
        case (nil, nil): nil
        }
    }

    /// The pointer is over drop `id`, or over no drop.
    public mutating func pointer(overDrop id: String?, at now: TimeInterval) {
        overDrop = id
        guard !pinned else { return }
        if let id {
            if id == shown {
                opening = nil
                openAt = nil
                closeAt = nil
            } else if id != opening {
                opening = id
                openAt = now + Self.openDelay
            }
        } else {
            opening = nil
            openAt = nil
            scheduleClose(at: now)
        }
    }

    /// The pointer moved onto or off the card.
    public mutating func pointer(overCard inside: Bool, at now: TimeInterval) {
        overCard = inside
        guard !pinned else { return }
        if inside {
            closeAt = nil
        } else if overDrop != shown {
            scheduleClose(at: now)
        }
    }

    /// Runs whatever came due by `now`.
    public mutating func tick(at now: TimeInterval) {
        if let at = openAt, now >= at, let id = opening {
            shown = id
            opening = nil
            openAt = nil
            closeAt = nil
        }
        if let at = closeAt, now >= at {
            closeAt = nil
            if !pinned, !overCard, overDrop != shown {
                shown = nil
            }
        }
    }

    /// A click on drop `id`: its card opens pinned.
    public mutating func click(drop id: String) {
        shown = id
        pinned = true
        opening = nil
        openAt = nil
        closeAt = nil
    }

    /// ×, Esc or a click elsewhere.
    public mutating func dismiss() {
        shown = nil
        pinned = false
        opening = nil
        openAt = nil
        closeAt = nil
    }

    private mutating func scheduleClose(at now: TimeInterval) {
        guard shown != nil, !overCard, closeAt == nil else { return }
        closeAt = now + Self.closeDelay
    }
}
