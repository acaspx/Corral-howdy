// CorralStopAllMenu.swift
// The corral's right-click menu, last part: Stop all agents (Recall-all, docs/07 §6). It's offered
// only while an agent is working, paused or waiting on you. Rust stops the runs (CorralControls::stop_all),
// dropping any that don't stop within 2 seconds; this only reports the click.

import SwiftUI

public struct CorralStopAllMenu: View {
    let agents: [CorralAgentToken]
    let onIntent: (CorralIntent) -> Void

    public init(agents: [CorralAgentToken], onIntent: @escaping (CorralIntent) -> Void) {
        self.agents = agents
        self.onIntent = onIntent
    }

    /// Whether anything is running that Stop all would stop. Pure, so `nonisolated`: a `View`'s
    /// members are main-actor isolated, and Swift 6 traps when one runs off the main thread (as
    /// Swift Testing does), taking every test running alongside it down too.
    nonisolated public static func isAvailable(agents: [CorralAgentToken]) -> Bool {
        agents.contains { $0.status == .working || $0.status == .paused || $0.status == .needsYou }
    }

    public var body: some View {
        if Self.isAvailable(agents: agents) {
            Divider()
            Button("Stop all agents", role: .destructive) { onIntent(.stopAll) }
        }
    }
}
