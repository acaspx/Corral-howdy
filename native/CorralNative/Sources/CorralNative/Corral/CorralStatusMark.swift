// CorralStatusMark.swift
// What a drop shows around its logo for each status (docs/07 §8.1). Status never relies on
// color alone: every mark has a shape, and VoiceOver reads the status in words.

import Foundation

public enum CorralStatusMark: Equatable, Sendable {
    /// In the corral: nothing extra.
    case none
    /// Working: a coral arc turning slowly (still under Reduce Motion).
    case turningArc
    /// Needs you: a maroon ring that pulses, with a badge.
    case pulse(badge: String)
    /// Done or error: a badge (an SF Symbol name).
    case badge(String)

    public static func of(_ status: CorralAgentToken.Status) -> CorralStatusMark {
        switch status {
        case .available: .none
        case .working: .turningArc
        case .needsYou: .pulse(badge: "exclamationmark")
        case .done: .badge("checkmark")
        case .error: .badge("exclamationmark.triangle.fill")
        }
    }
}
