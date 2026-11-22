// CorralTraceWireTests.swift: the hover card's plan and the trace tree, as the roster carries them
// (docs/07 §8.3, TP-HOVER-03, TP-TRACE-02, TP-TRACE-06). The core decides; Swift decodes it
// bounded and never traps.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func roster(_ body: String) -> CorralRoster? {
    try? CorralRoster.decode(Data(body.utf8)).get()
}

private let agents = #""agents":[{"id":"claude","name":"Claude","monogram":"Cl","status":"working"}],"hidden":0"#

private func card(plan: String) -> String {
    #"{"v":1,\#(agents),"cards":[{"id":"claude","status":"working","prompt":"p","promptTruncated":false,"elapsedMs":0,"spend":"$0.00","answerTruncated":false,"actions":["stop"],"plan":\#(plan)}]}"#
}

private func trace(_ columns: String, more: Int = 0) -> String {
    #"{"v":1,\#(agents),"trace":{"columns":[\#(columns)],"more":\#(more)}}"#
}

private func column(id: String = "claude", status: String = "working", step: Int = 2, total: Int = 5,
                    next: String = #"["a","b"]"#) -> String {
    #"{"id":"\#(id)","status":"\#(status)","prompt":"p","promptTruncated":false,"subtask":{"step":\#(step),"total":\#(total),"title":"Group"},"next":\#(next)}"#
}

@Test("TP-HOVER-03 a card's plan decodes from the fixture Rust sends")
func cardPlanDecodes() throws {
    let decoded = try CorralRoster.decode(fixture("roster-plan-v1.json")).get()
    let plan = try #require(decoded.cards["claude"]?.plan)
    #expect(plan.step == 2 && plan.total == 5)
    #expect(plan.now == "Group")
    #expect(plan.segments == [.done, .current, .pending, .pending, .pending])
    let plain = try CorralRoster.decode(fixture("roster-cards-v1.json")).get()
    #expect(plain.cards.values.allSatisfy { $0.plan == nil })
}

@Test("TP-HOVER-03 a plan Swift can't make sense of is dropped; the card stays")
func badPlansAreDropped() {
    for bad in [
        #"{"step":6,"total":5,"segments":["done","done","done","done","done"]}"#,
        #"{"step":0,"total":1,"segments":["current"]}"#,
        #"{"total":2,"segments":["done"]}"#,
        #"{"total":1,"segments":["asleep"]}"#,
    ] {
        let decoded = roster(card(plan: bad))
        #expect(decoded?.cards["claude"] != nil, "\(bad)")
        #expect(decoded?.cards["claude"]?.plan == nil, "\(bad)")
    }
    let long = String(repeating: "x", count: 500) + "\u{202e}"
    let plan = roster(card(plan: #"{"step":1,"total":1,"now":"\#(long)","segments":["current"]}"#))?.cards["claude"]?.plan
    #expect(plan?.now?.count == CorralRunCard.Plan.maxTitleLength)
    #expect(plan?.now?.contains("\u{202e}") == false)
    #expect(roster(card(plan: #"{"total":1,"segments":["done"],"extra":1}"#)) == nil, "unknown keys reject the roster")
}

@Test("TP-TRACE-02 the trace tree decodes from the fixture Rust sends")
func traceDecodes() throws {
    let decoded = try CorralRoster.decode(fixture("roster-trace-v1.json")).get()
    #expect(decoded.trace.more == 0)
    let column = try #require(decoded.trace.columns.first)
    #expect(column.id == "claude" && column.status == .working)
    #expect(column.prompt == "Tidy my notes" && !column.promptTruncated)
    #expect(column.subtask == CorralTrace.Subtask(step: 2, total: 5, title: "Group"))
    #expect(column.next == ["Summarize", "Email"])
}

@Test("TP-TRACE-06 a roster without a trace has no columns")
func noTraceNoColumns() throws {
    let decoded = try CorralRoster.decode(fixture("roster-v1.json")).get()
    #expect(decoded.trace.columns.isEmpty && decoded.trace.more == 0)
}

@Test("TP-TRACE-02 the trace is bounded: 3 columns, 2 peeking, valid IDs once, known statuses")
func traceIsBounded() {
    let five = ["a", "b", "c", "d", "e"].map { column(id: $0) }.joined(separator: ",")
    let t = roster(trace(five, more: 4))?.trace
    #expect(t?.columns.map(\.id) == ["a", "b", "c"])
    #expect(t?.more == 6, "columns past 3 count as more")
    let odd = [column(id: "has space"), column(id: "x", status: "asleep"), column(id: "y"), column(id: "y"),
               column(id: "z", step: 9, total: 2), column(id: "w", next: #"["1","2","3","4"]"#)]
        .joined(separator: ",")
    let kept = roster(trace(odd))?.trace
    #expect(kept?.columns.map(\.id) == ["y", "z", "w"])
    #expect(kept?.columns[1].subtask == nil, "a subtask past its total is dropped")
    #expect(kept?.columns[2].next.count == CorralTrace.maxPeek)
    #expect(roster(trace(column(), more: -3))?.trace.more == 0)
    #expect(roster(trace(column(id: "needs", status: "needsYou")))?.trace.columns.first?.status == .needsYou)
    #expect(roster(trace(column(id: "q", status: "done")))?.trace.columns.isEmpty == true,
            "only running columns")
}

@Test("TP-TRACE-02 a column carries its agent's name and logo when Rust sends them; a bad logo is dropped")
func columnNameAndLogo() {
    let named = #"{"id":"muse","name":"Muse ‮Spark","logo":"/x/logos/muse.png","status":"working","prompt":"p","promptTruncated":false,"next":[]}"#
    let column = roster(trace(named))?.trace.columns.first
    #expect(column?.name == "Muse Spark")
    #expect(column?.logoPath == "/x/logos/muse.png")
    let bad = #"{"id":"muse","logo":"/etc/passwd","status":"working","prompt":"p","promptTruncated":false,"next":[]}"#
    let plain = roster(trace(bad))?.trace.columns.first
    #expect(plain != nil && plain?.name == nil && plain?.logoPath == nil)
}
