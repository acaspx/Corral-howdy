// CorralJourneyTests.swift: end-to-end journeys, Swift side (Spike S2, docs/04-test-plan.md §6).
// Each transcript in Fixtures/journeys is recorded by the Rust core playing the journey
// (apps/desktop/src-tauri/tests/journeys.rs): the gestures, the intent each must send, the
// rosters and prompt bar answers the core sent back, and what the corral should show after each
// step. This replays it into CorralNative's real decoding and models: every roster and bar answer
// must decode, each gesture performed on the corral's own models (the card, its approval box and
// edit sheet, the prompt bar) must send exactly the recorded intent, and the shape, the drops in
// slot order with their status, the trace columns, the approval box and the bar must be what the
// step expects. A step done in the main window (its `window`) is the Vitest journey test's to
// perform (apps/desktop/src/journeys.test.tsx); here, only its rosters are taken.
import Testing
import Foundation
@testable import CorralNative

private enum ReplayError: Error {
    case unknownGesture(String)
}

private func transcript(_ journey: String) throws -> [String: Any] {
    // Tests/CorralNativeTests/<this file> → package root → Fixtures/journeys/<journey>.json
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    let data = try Data(contentsOf: root.appendingPathComponent("Fixtures/journeys/\(journey).json"))
    return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
}

/// The shape a transcript names, in the corral's grammar (docs/07 §3).
private func shape(named name: String) -> CorralShape? {
    switch name {
    case "empty": .empty
    case "circle": .circle
    case "dumbbell": .dumbbell
    case "triangle": .polygon(sides: 3)
    case "square": .polygon(sides: 4)
    case "pentagon": .polygon(sides: 5)
    case "hexagon": .polygon(sides: 6)
    case "heptagon": .polygon(sides: 7)
    case "octagon": .polygon(sides: 8)
    default: nil
    }
}

/// A drop's status as the wire names it.
private func wireName(_ status: CorralAgentToken.Status) -> String {
    switch status {
    case .available: "available"
    case .working: "working"
    case .paused: "paused"
    case .needsYou: "needsYou"
    case .done: "done"
    case .error: "error"
    }
}

/// A checkpoint's risk as the wire names it.
private func wireName(_ risk: CorralRunCard.Checkpoint.Risk) -> String {
    switch risk {
    case .normal: "normal"
    case .high: "high"
    case .critical: "critical"
    }
}

/// A send's outcome as the wire names its kind, or nil for an edit's answer.
private func wireName(_ outcome: CorralPromptBarOutcome?) -> String? {
    switch outcome {
    case nil: nil
    case .sent?: "sent"
    case .note?: "note"
    case .refused?: "refused"
    }
}

/// A card button a transcript names, for a card with no checkpoint waiting.
private func plainButton(named name: String) -> CorralCardModel.Button? {
    switch name {
    case "stop": .stop
    case "pause": .pause
    case "resume": .resume
    case "sendBack": .sendBack
    case "retry": .retry
    default: nil
    }
}

private func isApprove(_ button: CorralCardModel.Button) -> Bool {
    if case .approve = button { return true }
    return false
}

private func isDeny(_ button: CorralCardModel.Button) -> Bool {
    if case .deny = button { return true }
    return false
}

private func json(_ object: Any) throws -> NSDictionary? {
    let data = try JSONSerialization.data(withJSONObject: object)
    return try JSONSerialization.jsonObject(with: data) as? NSDictionary
}

/// What the replay keeps between steps: the latest roster, and the prompt bar with the drops
/// ⌘-clicked.
@MainActor
private final class Replay {
    var roster: CorralRoster?
    var barState = CorralPromptBarState()
    let bar = CorralPromptBar()
    var selection: [String] = []
    /// The pointer over the corral, and what Rust has been told of its idle state.
    var hover = CorralHoverState()
    var idleReporter = CorralIdleReporter()
    var clock: TimeInterval = 1_000

    /// The card model the corral builds for `agent`'s drop.
    func card(_ agent: String) throws -> CorralCardModel {
        let roster = try #require(roster, "a gesture before the corral opened")
        let token = try #require(roster.agents.first { $0.id == agent }, "\(agent) isn't in the corral")
        return CorralCardModel(agentName: token.name, card: roster.cards[agent], linkedApp: token.linkedApp)
    }

    /// What the user's gesture sends, performed on the corral's own models.
    func perform(_ gesture: [String: Any]) throws -> CorralIntent {
        let kind = gesture["kind"] as? String ?? "?"
        let agent = gesture["agent"] as? String ?? ""
        let pinned = gesture["pinned"] as? Bool ?? false
        switch kind {
        case "task":
            // Return in the card's field with no checkpoint waiting: the text is a task.
            let text = try #require(gesture["text"] as? String)
            return try #require(try card(agent).fieldIntent(text, agentID: agent), "\(agent)'s card takes no task")
        case "note":
            // Return in the card's field while a checkpoint waits: Deny, with the text as its note.
            let model = try card(agent)
            try #require(model.approval != nil, "nothing waits on \(agent)'s card")
            let text = try #require(gesture["text"] as? String)
            return try #require(model.fieldIntent(text, agentID: agent))
        case "press":
            let name = try #require(gesture["button"] as? String)
            let model = try card(agent)
            let pressed = try button(named: name, on: model, pinned: pinned)
            return CorralCardModel.intent(for: pressed, agentID: agent)
        case "edit":
            // The edit sheet: the agent's fields, with the changes typed in, then Approve edited.
            let approval = try #require(try card(agent).approval, "nothing waits on \(agent)'s card")
            #expect(approval.editable, "the sheet has a field to change")
            let changes = try #require(gesture["changes"] as? [String: String])
            var draft = Dictionary(uniqueKeysWithValues: approval.fields.map { ($0.key, $0.value) })
            draft.merge(changes) { _, typed in typed }
            #expect(approval.editHint(draft, pinned: pinned) == nil, "Approve edited can be pressed")
            let pressed = try #require(approval.approveEdited(draft))
            return CorralCardModel.intent(for: pressed, agentID: agent)
        case "openBar":
            // ⌘-click the drops (the corral reports them in its order), then open the bar.
            let roster = try #require(roster)
            let picked = Set(try #require(gesture["select"] as? [String]))
            selection = CorralGlassView.ordered(picked, in: roster.agents.map(\.id))
            #expect(barState.open(naming: nil) == .edited)
            return bar.edited(text: barState.text, utf16Cursor: barState.cursor, selection: selection)
        case "typeInBar":
            let text = try #require(gesture["text"] as? String)
            #expect(barState.edit(text: text, cursor: text.utf16.count) == .edited)
            return bar.edited(text: barState.text, utf16Cursor: barState.cursor, selection: selection)
        case "sendBar":
            // Return in the bar with no suggestion list: the bar's send button. ⌘↩ sends to every
            // agent.
            let all = gesture["all"] as? Bool ?? false
            let key: CorralPromptBarState.Key = all ? .commandEnter : .enter
            #expect(barState.key(key, suggestions: bar.view?.suggestions ?? []) == .send(all: all))
            return bar.send(text: barState.text, selection: selection, all: all)
        case "pointAt":
            // The pointer rests on a drop until its card opens: the corral isn't idle.
            hover.pointer(overDrop: agent, at: clock)
            clock += CorralHoverState.openDelay
            hover.tick(at: clock)
            #expect(hover.shown == agent, "\(agent)'s card opens")
            let report = idleReporter.report(hover.isIdle)
            return .corralIdle(try #require(report, "the corral's idle state changed"))
        case "leaveCorral":
            // The pointer leaves the drop and the card, and the card closes: the corral is idle.
            hover.pointer(overDrop: nil, at: clock)
            hover.pointer(overCard: false, at: clock)
            clock += CorralHoverState.closeDelay
            hover.tick(at: clock)
            #expect(hover.isIdle, "the card closed")
            let report = idleReporter.report(hover.isIdle)
            return .corralIdle(try #require(report, "the corral's idle state changed"))
        case "stopAll":
            // The corral's right-click menu, which offers Stop all agents only while one works.
            let roster = try #require(roster)
            #expect(CorralStopAllMenu.isAvailable(agents: roster.agents), "the menu offers Stop all agents")
            return .stopAll
        default:
            throw ReplayError.unknownGesture(kind)
        }
    }

    /// The button pressed on a card. Approve and Allow for this run go through the approval
    /// box's own rules: a high-risk card has to be pinned first.
    func button(named name: String, on model: CorralCardModel, pinned: Bool) throws -> CorralCardModel.Button {
        if let plain = plainButton(named: name) {
            try #require(model.buttons.contains(plain), "the card has no \(name) button")
            return plain
        }
        let approval = try #require(model.approval, "nothing waits on the card")
        if pinned, approval.risk != .normal {
            #expect(!approval.canApprove(pinned: false, expanded: true), "it can't be approved unpinned")
        }
        switch name {
        case "approve":
            #expect(approval.canApprove(pinned: pinned, expanded: true), "Approve can be pressed")
            return try #require(model.buttons.first(where: isApprove), "the card has no Approve button")
        case "deny":
            return try #require(model.buttons.first(where: isDeny), "the card has no Deny button")
        case "allowForRun":
            #expect(approval.forRunTitle != nil, "Allow for this run is offered")
            #expect(approval.canApprove(pinned: pinned, expanded: true), "it can be pressed")
            return .allowForRun(checkpoint: approval.checkpointID)
        default:
            throw ReplayError.unknownGesture(name)
        }
    }

    /// Takes the step's rosters and bar answers, as the corral and the bar would.
    func receive(_ step: [String: Any], say: String) throws {
        let rosters = step["rosters"] as? [Any] ?? []
        for sent in rosters {
            let data = try JSONSerialization.data(withJSONObject: sent)
            switch CorralRoster.decode(data) {
            case .success(let decoded): roster = decoded
            case .failure(let error): Issue.record("\(say): a roster didn't decode: \(error)")
            }
        }
        for sent in step["bars"] as? [Any] ?? [] {
            let data = try JSONSerialization.data(withJSONObject: sent)
            switch CorralPromptBarSnapshot.decode(data) {
            case .success(let answer):
                #expect(bar.receive(answer), "\(say): the bar's answer is for its latest edit or send")
                barState.answered(answer.outcome)
            case .failure(let error): Issue.record("\(say): a bar answer didn't decode: \(error)")
            }
        }
    }

    /// Checks what the corral and the bar show against the step's expectations.
    func check(_ expect: [String: Any], say: String) throws {
        let shown = try #require(roster)

        let named = try #require(expect["shape"] as? String)
        let wanted = try #require(shape(named: named), "\(say): no shape \(named)")
        #expect(CorralLayout.shape(forShownCount: shown.agents.count) == wanted, "\(say): the shape")
        let layout = CorralLayout.layout(rankedIDs: shown.agents.map(\.id))
        #expect(layout.shape == wanted, "\(say): the layout's shape")

        let drops = try #require(expect["drops"] as? [[String: String]])
        #expect(shown.agents.map(\.id) == drops.compactMap { $0["id"] }, "\(say): the drops in slot order")
        #expect(shown.agents.map { wireName($0.status) } == drops.compactMap { $0["status"] },
                "\(say): each drop's status")
        let ranked = layout.blobs.filter { $0.rank > 0 }.sorted { $0.rank < $1.rank }.map(\.id)
        #expect(ranked == drops.compactMap { $0["id"] }, "\(say): rank 1 first, clockwise")

        let columns = try #require(expect["columns"] as? [String])
        #expect(shown.trace.columns.map(\.id) == columns, "\(say): the trace columns")
        if let more = expect["moreColumns"] as? Int {
            #expect(shown.trace.more == more, "\(say): \"+\(more) more running\"")
        }
        if let offered = expect["stopAllOffered"] as? Bool {
            #expect(CorralStopAllMenu.isAvailable(agents: shown.agents) == offered,
                    "\(say): the menu offers Stop all agents")
        }
        if let statuses = expect["columnStatus"] as? [String: String] {
            for (id, status) in statuses {
                let column = shown.trace.columns.first { $0.id == id }
                #expect(column?.status.rawValue == status, "\(say): \(id)'s column")
            }
        }

        if let wanted = expect["approval"] as? [String: Any] {
            let agent = try #require(wanted["agent"] as? String)
            let approval = try #require(try card(agent).approval, "\(say): no approval box on \(agent)'s card")
            #expect(approval.classLabel == wanted["class"] as? String, "\(say): the class")
            #expect(wireName(approval.risk) == wanted["risk"] as? String, "\(say): the risk")
            for text in wanted["detail"] as? [String] ?? [] {
                #expect(approval.detail.contains(text), "\(say): the box says \(text)")
            }
            if let forRun = wanted["forRun"] as? Bool {
                #expect((approval.forRunTitle != nil) == forRun, "\(say): Allow for this run is offered")
            }
        }

        if let names = expect["names"] as? [String: String] {
            for (id, name) in names {
                #expect(shown.agents.first { $0.id == id }?.name == name, "\(say): \(id) is named \(name)")
            }
        }
        if let logos = expect["logos"] as? [String] {
            for id in logos {
                let path = shown.agents.first { $0.id == id }?.logoPath
                #expect(path != nil, "\(say): \(id) shows its logo (a path in a logos folder)")
            }
        }

        if let wanted = expect["bar"] as? [String: Any] {
            let view = try #require(bar.view, "\(say): the bar has no answer")
            #expect(view.chips == wanted["chips"] as? [String], "\(say): the chips")
            #expect(view.button.label == wanted["button"] as? String, "\(say): the button")
            let outcome = wanted["outcome"] as? String
            #expect(wireName(bar.outcome) == outcome, "\(say): how the send went")
            if outcome == "sent" {
                #expect(!barState.isOpen && barState.text.isEmpty, "\(say): the bar closes, its field empty")
            }
        }
    }
}

/// Replays `journey`: every step's gesture, rosters, bar answers and expectations.
@MainActor
private func replay(_ journey: String) throws {
    let recorded = try transcript(journey)
    let steps = try #require(recorded["steps"] as? [[String: Any]])
    #expect(!steps.isEmpty)
    let replay = Replay()
    for (index, step) in steps.enumerated() {
        let say = step["say"] as? String ?? "?"
        if let gesture = step["gesture"] as? [String: Any] {
            let intent = try replay.perform(gesture)
            let sent = try #require(CorralIntentWire.encode(intent))
            let expected = try #require(step["intent"] as? [String: Any], "\(say): no intent recorded")
            #expect(try JSONSerialization.jsonObject(with: sent) as? NSDictionary == json(expected),
                    "\(say): the gesture sends the recorded intent")
        }
        let rosters = step["rosters"] as? [Any] ?? []
        if index == 0 { #expect(!rosters.isEmpty, "\(say): the corral opens") }
        try replay.receive(step, say: say)
        try replay.check(try #require(step["expect"] as? [String: Any]), say: say)
    }
}

// MARK: The journeys

@Test("J9 the corral morphs as agents come and go: triangle, square, triangle, and back to ready")
@MainActor
func j9CorralMorph() throws {
    try replay("J9")
}

@Test("J1 first launch: a key, a logo, a name, a task, and Send back")
@MainActor
func j1FirstLaunch() throws {
    try replay("J1")
}

@Test("J5 a run at its soft limit asks on its card, and goes on when approved")
@MainActor
func j5SoftLimit() throws {
    try replay("J5")
}

@Test("J2 three drops ⌘-clicked and one task from the prompt bar: three columns, each on its own")
@MainActor
func j2ThreeAgentsAtOnce() throws {
    try replay("J2")
}

@Test("J3 a run's checkpoints: approve, deny with a note, edit, and allow for the run")
@MainActor
func j3AnsweringCheckpoints() throws {
    try replay("J3")
}

@Test("J6 Stop all from the corral's menu stops five runs, and each agent can go again")
@MainActor
func j6StopAll() throws {
    try replay("J6")
}

@Test("J10 the most used agent glides to the top-left once the corral is idle; a 9th asks which 8 show")
@MainActor
func j10UsageOrderAndShownSet() throws {
    try replay("J10")
}

@Test("J11 a message waits on you: the card shows where it goes; deny with a note, then approve pinned")
@MainActor
func j11HoverApproval() throws {
    try replay("J11")
}
