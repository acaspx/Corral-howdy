// CorralPromptBarPanelTests.swift: the prompt bar's behavior (docs/07 §6.1): opening and closing,
// the draft, the keys for the @ suggestions, and how a send's answer leaves the bar
// (TP-BAR-01, 04, 05, 06). `CorralPromptBarState` is pure, so these need no window.
import Testing
import Foundation
import CoreGraphics
@testable import CorralNative

private func row(_ id: String, _ name: String, _ completion: String) -> CorralPromptBarView.Suggestion {
    .init(id: id, name: name, shown: true, status: .ready, completion: completion)
}

private let rows = [row("claude", "Claude", "@Claude "), row("cohere", "Cohere", "@Cohere ")]

@Test("TP-BAR-01 opening asks Rust for the view; dragging a drop up opens it with that agent named")
func barOpens() {
    var bar = CorralPromptBarState()
    #expect(!bar.isOpen)
    #expect(bar.open(naming: nil) == .edited)
    #expect(bar.isOpen && bar.text.isEmpty)
    bar.close()
    #expect(bar.open(naming: "Grok") == .edited)
    #expect(bar.text == "@Grok " && bar.cursor == 6, "the cursor after the name")
    #expect(bar.open(naming: "Grok") == .edited)
    #expect(bar.text == "@Grok ", "not named twice")
}

@Test("TP-BAR-06 closing without sending keeps the text; ⌘⌫ clears it")
func draftIsKept() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    #expect(bar.edit(text: "plan the trip", cursor: 13) == .edited)
    #expect(bar.key(.escape, suggestions: []) == .close)
    #expect(!bar.isOpen && bar.text == "plan the trip")
    _ = bar.open(naming: nil)
    #expect(bar.text == "plan the trip")
    _ = bar.open(naming: "Claude")
    #expect(bar.text == "@Claude plan the trip", "a drag names the agent before the draft")
    #expect(bar.key(.clear, suggestions: []) == .edited)
    #expect(bar.text.isEmpty && bar.cursor == 0)
    #expect(bar.edit(text: "", cursor: 0) == .none, "nothing changed")
}

@Test("TP-BAR-04 ↑↓ move through the suggestions; Return or Tab takes one; Esc closes the list, then the bar")
func suggestionKeys() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    _ = bar.edit(text: "@c plan", cursor: 2)
    #expect(bar.listShown(rows.count))
    #expect(bar.key(.down, suggestions: rows) == .none)
    #expect(bar.highlighted == 1)
    #expect(bar.key(.down, suggestions: rows) == .none)
    #expect(bar.highlighted == 1, "stays on the last")
    #expect(bar.key(.up, suggestions: rows) == .none)
    #expect(bar.key(.up, suggestions: rows) == .none)
    #expect(bar.highlighted == 0, "stays on the first")
    #expect(bar.key(.enter, suggestions: rows) == .edited, "Return takes the row, it doesn't send")
    #expect(bar.text == "@Claude  plan" && bar.cursor == 8, "the completion, then the rest")

    _ = bar.edit(text: "@c", cursor: 2)
    _ = bar.key(.down, suggestions: rows)
    #expect(bar.key(.tab, suggestions: rows) == .edited)
    #expect(bar.text == "@Cohere ")

    _ = bar.edit(text: "@c", cursor: 2)
    #expect(bar.key(.escape, suggestions: rows) == .none)
    #expect(bar.isOpen && !bar.listShown(rows.count), "Esc closes the list, not the bar")
    #expect(bar.key(.down, suggestions: rows) == .toCorral, "no list: ↓ goes down into the corral (TP-A11Y-02)")
    _ = bar.edit(text: "@cl", cursor: 3)
    #expect(bar.listShown(rows.count), "typing shows it again")
    #expect(bar.key(.tab, suggestions: []) == .pass, "no rows: Tab moves focus")
    _ = bar.key(.escape, suggestions: rows)
    #expect(bar.key(.escape, suggestions: rows) == .close)
}

@Test("TP-BAR-04 a highlight past the rows Rust sent lands on the last one")
func highlightIsClamped() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    _ = bar.edit(text: "@c", cursor: 2)
    _ = bar.key(.down, suggestions: rows)
    let one = [rows[0]]
    #expect(bar.key(.enter, suggestions: one) == .edited)
    #expect(bar.text == "@Claude ")
}

@Test("TP-BAR-05 Return sends once; the field waits for Rust's answer, and a refusal keeps the text")
func sendWaitsForTheAnswer() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    _ = bar.edit(text: "@nobody go", cursor: 10)
    #expect(bar.key(.enter, suggestions: []) == .send(all: false))
    #expect(bar.sending)
    #expect(bar.key(.enter, suggestions: []) == .none, "not twice")
    #expect(bar.edit(text: "@nobody go!", cursor: 11) == .none, "typing waits for the answer")
    bar.answered(.refused("No agent is called nobody."))
    #expect(!bar.sending && bar.isOpen)
    #expect(bar.text == "@nobody go")
    #expect(bar.message == .refused("No agent is called nobody."))
    _ = bar.edit(text: "@grok go", cursor: 8)
    #expect(bar.message == nil, "the next edit clears it")
}

@Test("TP-BAR-05 a sent prompt closes the bar; a note keeps it open with the text cleared")
func sentAndNoted() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    _ = bar.edit(text: "@grok go", cursor: 8)
    #expect(bar.key(.commandEnter, suggestions: rows) == .send(all: true), "⌘Return is Deploy all")
    bar.answered(.sent)
    #expect(!bar.isOpen && bar.text.isEmpty && !bar.sending && bar.message == nil)

    _ = bar.open(naming: nil)
    _ = bar.edit(text: "@grok @claude go", cursor: 16)
    _ = bar.key(.enter, suggestions: [])
    bar.answered(.note("Sent to Claude. Grok is busy."))
    #expect(bar.isOpen && bar.text.isEmpty)
    #expect(bar.message == .note("Sent to Claude. Grok is busy."))

    bar.answered(nil)
    #expect(bar.message == .note("Sent to Claude. Grok is busy."), "an edit's answer changes nothing here")
}

@Test("TP-BAR-06 closing while a send waits lets the next opening type again")
func closingWhileSending() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    _ = bar.edit(text: "go", cursor: 2)
    _ = bar.key(.enter, suggestions: [])
    #expect(bar.key(.escape, suggestions: []) == .close)
    _ = bar.open(naming: nil)
    #expect(!bar.sending)
    #expect(bar.edit(text: "go on", cursor: 5) == .edited)
}

@Test("TP-BAR-01 the bar sits 8 pt above the corral, centered, 480 to 640 pt wide, on screen")
func barPlacement() {
    let screen = CGRect(x: 0, y: 0, width: 1440, height: 900)
    let corral = CGRect(x: 620, y: 20, width: 200, height: 120)
    let f = CorralPromptBarController.frame(height: 60, corral: corral, visible: screen)
    #expect(f.width == 480 && f.height == 60)
    #expect(f.minY == corral.maxY + 8)
    #expect(f.midX == corral.midX)

    let wide = CorralPromptBarController.frame(height: 60, corral: CGRect(x: 100, y: 20, width: 900, height: 120), visible: screen)
    #expect(wide.width == 640)
    let edge = CorralPromptBarController.frame(height: 60, corral: CGRect(x: 0, y: 20, width: 100, height: 120), visible: screen)
    #expect(edge.minX == 8, "clamped to the screen")
    let small = CorralPromptBarController.frame(height: 60, corral: corral, visible: CGRect(x: 0, y: 0, width: 400, height: 900))
    #expect(small.width == 384 && small.minX == 8, "never wider than the screen")
    let low = CorralPromptBarController.frame(height: 400, corral: CGRect(x: 620, y: 700, width: 200, height: 120), visible: screen)
    #expect(low.maxY == screen.maxY - 8, "kept below the top")
}

@Test("TP-BAR-05 the line under the bar: a send's answer until the next edit, else the core's error, with a glyph")
func lineUnderTheBar() {
    let line = CorralPromptBarRoot.lineContent(message:error:)
    typealias Line = CorralPromptBarRoot.Line
    #expect(line(nil, nil) == nil)
    #expect(line(nil, "No agent is called x.") == Line(glyph: "exclamationmark.circle", text: "No agent is called x.", isError: true))
    #expect(line(.refused("Grok is busy."), "ignored") == Line(glyph: "exclamationmark.circle", text: "Grok is busy.", isError: true))
    #expect(line(.note("Sent to Claude. Grok is busy."), nil) == Line(glyph: "info.circle", text: "Sent to Claude. Grok is busy.", isError: false))
}

@Test("TP-BAR-07 VoiceOver reads who it goes to, and each suggestion with its status and hidden")
func barAccessibility() {
    #expect(CorralPromptBarRoot.andList(["Grok"]) == "Grok")
    #expect(CorralPromptBarRoot.andList(["Grok", "Claude", "GPT"]) == "Grok, Claude and GPT")
    #expect(CorralPromptBarRoot.rowLabel(CorralPromptBarView.Suggestion(id: "g", name: "Grok", shown: true, status: .ready, completion: "@Grok ")) == "Grok")
    #expect(CorralPromptBarRoot.rowLabel(CorralPromptBarView.Suggestion(id: "m", name: "Muse Spark", shown: false, status: .needsYou, completion: "@Muse Spark ")) == "Muse Spark, Needs you, hidden")
}

@Test("TP-BAR-04 a click takes any row")
func clickTakesARow() {
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    _ = bar.edit(text: "@c", cursor: 2)
    #expect(bar.choose(5, suggestions: rows) == .none, "no such row")
    #expect(bar.choose(1, suggestions: rows) == .edited)
    #expect(bar.text == "@Cohere ")
    _ = bar.edit(text: "@c", cursor: 2)
    _ = bar.key(.escape, suggestions: rows)
    #expect(bar.choose(0, suggestions: rows) == .none, "the list is closed")
}

@Test("TP-BAR-02 the prompt bar gets the ⌘-clicked drops in the corral's order, without ones that left")
func selectionOrder() {
    #expect(CorralGlassView.ordered(["gpt", "grok", "gone"], in: ["grok", "claude", "gpt"]) == ["grok", "gpt"])
    #expect(CorralGlassView.ordered([], in: ["grok"]).isEmpty)
}
