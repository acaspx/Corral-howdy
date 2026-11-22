// CorralTraceTreeTests.swift: the hover card's progress and the trace tree above the corral
// (docs/07 §8.2, §8.3; TP-HOVER-03, TP-TRACE-02..04). The core chose what shows; these check where
// Swift puts it.
import Testing
import Foundation
import CoreGraphics
@testable import CorralNative

private func card(_ plan: CorralRunCard.Plan?, status: CorralRunCard.Status = .working) -> CorralCardModel {
    CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: status, prompt: "Tidy my notes", elapsedMs: 76_000, spend: "$0.03", actions: [.stop],
        plan: plan))
}

@Test("TP-HOVER-03 a plan gives the status line its step, a segment per subtask, and the Now line")
func cardProgress() {
    let plan = CorralRunCard.Plan(step: 2, total: 5, now: "Group",
                                  segments: [.done, .current, .pending, .pending, .skipped])
    let working = card(plan)
    #expect(working.status == "Step 2 of 5 · 1m 16s")
    #expect(working.progress?.segments == plan.segments)
    #expect(working.progress?.now == "Now: Group · 1m 16s · $0.03")
    #expect(working.progress?.accessibilityLabel == "Step 2 of 5. 1 done, 1 skipped.")

    let waiting = card(CorralRunCard.Plan(step: 5, total: 5, now: "Email",
                                          segments: [.done, .done, .done, .failed, .current]), status: .needsYou)
    #expect(waiting.status == "Needs your approval · step 5 of 5")
    #expect(waiting.progress?.accessibilityLabel == "Step 5 of 5. 3 done, 1 failed.")

    let between = card(CorralRunCard.Plan(step: nil, total: 2, now: nil, segments: [.done, .pending]))
    #expect(between.status == "Working · 1m 16s · $0.03", "no current step: the plain line")
    #expect(between.progress?.now == nil)
    #expect(between.progress?.accessibilityLabel == "2 steps. 1 done.")

    #expect(card(nil).progress == nil)
    #expect(card(nil).status == "Working · 1m 16s · $0.03", "no plan: as before")
    let done = card(CorralRunCard.Plan(step: nil, total: 1, now: nil, segments: [.done]), status: .done)
    #expect(done.status == "Done in 1m 16s · $0.03")
}

@Test("TP-TRACE-03 each wire ends at its drop, after a reorder too; a hidden agent's at the corral's end")
func wireAnchors() {
    let corral = CGRect(x: 500, y: 20, width: 400, height: 120)
    for ids in [["grok", "claude", "gpt"], ["gpt", "grok", "claude"]] {
        let layout = CorralLayout.layout(rankedIDs: ids)
        let drops = CorralTraceLayout.dropXs(rankedIDs: ids, corralMinX: corral.minX)
        for blob in layout.blobs where ids.contains(blob.id) {
            #expect(drops[blob.id] == corral.minX + layout.pivot.x + blob.offset.x, "\(ids) \(blob.id)")
        }
        #expect(CorralTraceLayout.anchorX(for: "claude", drops: drops, corral: corral) == drops["claude"])
    }
    #expect(CorralTraceLayout.anchorX(for: "muse", drops: [:], corral: corral)
        == corral.maxX - CorralTraceLayout.hiddenInset)
}

@Test("TP-TRACE-02 columns sit side by side above the corral, or above the open prompt bar, on screen")
func columnPlacement() {
    let step = CorralTraceLayout.columnWidth + CorralTraceLayout.columnGap
    #expect(CorralTraceLayout.columnCenters(count: 1, width: 600) == [300])
    #expect(CorralTraceLayout.columnCenters(count: 3, width: 800) == [400 - step, 400, 400 + step])
    #expect(CorralTraceLayout.columnCenters(count: 0, width: 800).isEmpty)

    let screen = CGRect(x: 0, y: 0, width: 1440, height: 900)
    let corral = CGRect(x: 500, y: 20, width: 400, height: 120)
    let three = CorralTraceLayout.frame(columns: 3, height: 300, corral: corral, above: corral.maxY, visible: screen)
    #expect(three.minY == corral.maxY && three.midX == corral.midX)
    #expect(three.width == 3 * CorralTraceLayout.columnWidth + 2 * CorralTraceLayout.columnGap)
    let wide = CorralTraceLayout.frame(columns: 1, height: 300, corral: CGRect(x: 100, y: 20, width: 900, height: 120),
                                       above: 140, visible: screen)
    #expect(wide.width == 900, "wide enough for every drop's wire")
    let overBar = CorralTraceLayout.frame(columns: 1, height: 300, corral: corral, above: 260, visible: screen)
    #expect(overBar.minY == 260)
    let tall = CorralTraceLayout.frame(columns: 1, height: 2_000, corral: corral, above: corral.maxY, visible: screen)
    #expect(tall.maxY == screen.maxY - 8, "kept below the top")
    let narrow = CorralTraceLayout.frame(columns: 3, height: 300, corral: corral,
                                         above: corral.maxY, visible: CGRect(x: 0, y: 0, width: 500, height: 900))
    #expect(narrow.width == 484 && narrow.minX == 8)
}

@Test("TP-TRACE-03 a wire drops from its column, runs across halfway down, and drops to its drop")
func wireCorners() {
    #expect(CorralTraceLayout.wire(from: CGPoint(x: 100, y: 0), to: CGPoint(x: 300, y: 40))
        == [CGPoint(x: 100, y: 0), CGPoint(x: 100, y: 20), CGPoint(x: 300, y: 20), CGPoint(x: 300, y: 40)])
    #expect(CorralTraceLayout.wire(from: CGPoint(x: 100, y: 0), to: CGPoint(x: 110, y: 40))
        == [CGPoint(x: 100, y: 0), CGPoint(x: 110, y: 40)], "too close for elbows: straight")
}

@Test("TP-TRACE-04 nothing running folds the tree away; the wires hide while the prompt bar is open")
func treeFolds() {
    let one = CorralTrace(columns: [.init(id: "grok", status: .working, prompt: "p", promptTruncated: false,
                                          subtask: nil, next: [])], more: 0)
    #expect(CorralTraceLayout.shows(one))
    #expect(!CorralTraceLayout.shows(.empty))
    #expect(CorralTraceLayout.shows(CorralTrace(columns: [], more: 0)) == false)
    #expect(CorralTraceLayout.showsWires(barOpen: false))
    #expect(!CorralTraceLayout.showsWires(barOpen: true))
}

@Test("TP-TRACE-02 VoiceOver reads a column: the agent, its status, the task and the current subtask")
func columnLabel() {
    var column = CorralTrace.Column(id: "claude", status: .needsYou, prompt: "Tidy my notes", promptTruncated: false,
                                    subtask: CorralTrace.Subtask(step: 2, total: 5, title: "Group"), next: [],
                                    name: "Claude")
    #expect(CorralTraceColumnView.label(column) == "Claude, Needs you: Tidy my notes. Subtask 2 of 5: Group.")
    column.subtask = nil
    column.name = nil
    column.status = .queued
    #expect(CorralTraceColumnView.label(column) == "Agent, Waiting to start: Tidy my notes.")
}
