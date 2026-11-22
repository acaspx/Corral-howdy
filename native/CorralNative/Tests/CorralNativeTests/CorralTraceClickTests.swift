// CorralTraceClickTests.swift: clicks on the trace tree (docs/07 §8.3, TP-TRACE-05). A task card
// opens its run in the main window's trace viewer (Rust finds the run); a click on the subtask
// stack fans it out, and again folds it.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func json(_ data: Data) throws -> NSDictionary? {
    try JSONSerialization.jsonObject(with: data) as? NSDictionary
}

@Test("TP-TRACE-05 a task card's click is reported as the fixture Rust decodes")
func openTraceEncodes() throws {
    let sent = try #require(CorralIntentWire.encode(.openTrace(id: "claude")))
    #expect(try json(sent) == json(fixture("intent-open-trace-v1.json")))
}

@Test("TP-TRACE-05 a click on a subtask stack fans it out, and again folds it")
func stackFans() {
    var fanned = Set<String>()
    fanned = CorralTraceLayout.toggled(fanned, "claude")
    #expect(fanned == ["claude"])
    fanned = CorralTraceLayout.toggled(fanned, "grok")
    #expect(fanned == ["claude", "grok"])
    fanned = CorralTraceLayout.toggled(fanned, "claude")
    #expect(fanned == ["grok"])
}

@Test("TP-TRACE-05 a column that has gone stops being fanned")
func fannedFollowsTheColumns() {
    #expect(CorralTraceLayout.kept(["claude", "gone"], columns: ["claude", "grok"]) == ["claude"])
}

@Test("TP-TRACE-05 VoiceOver reads a stack with what's left in it")
func stackLabel() {
    let subtask = CorralTrace.Subtask(step: 2, total: 5, title: "Group")
    var column = CorralTrace.Column(id: "claude", status: .working, prompt: "p", promptTruncated: false,
                                    subtask: subtask, next: ["Summarize", "Email"])
    #expect(CorralTraceColumnView.stackLabel(column, subtask: subtask)
        == "Subtask 2 of 5: Group. Next: Summarize, Email.")
    column.next = []
    #expect(CorralTraceColumnView.stackLabel(column, subtask: subtask) == "Subtask 2 of 5: Group.")
}
