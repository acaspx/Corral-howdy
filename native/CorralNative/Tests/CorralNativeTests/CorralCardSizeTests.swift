// CorralCardSizeTests.swift: TP-HOVER-11, TP-PLACE-06 (docs/04-test-plan.md).
// The card opened at 0×0, invisible, because its hosting view doesn't size its window and then
// reports no fitting size. The card is measured with SwiftUI instead, and Corral's panels act on
// the first click even while another app is in front.
import AppKit
import SwiftUI
import Testing
@testable import CorralNative

@MainActor
private func cardView(_ card: CorralRunCard?) -> AnyView {
    AnyView(CorralCardView(
        model: CorralCardModel(agentName: "Scout", card: card),
        pinned: false,
        onButton: { _ in }, onTask: { _ in }, onClose: {}, onPointerInside: { _ in }))
}

@Test("TP-HOVER-11 a card measures to a visible size, idle and working")
@MainActor
func cardMeasuresToAVisibleSize() {
    let working = CorralRunCard(status: .working, prompt: "Tidy my notes.", elapsedMs: 1_000,
                                spend: "$0.01", actions: [.stop])
    for card in [nil, working] {
        let size = CorralCardController.measure(cardView(card))
        #expect(size.width >= CorralCardView.width, "\(size)")
        #expect(size.height >= 80, "\(size)")
    }
}

@Test("TP-HOVER-11 a longer card measures taller")
@MainActor
func longerCardMeasuresTaller() {
    let short = CorralRunCard(status: .working, prompt: "Hi.", elapsedMs: 0, spend: "$0.00",
                              actions: [.stop])
    let long = CorralRunCard(status: .working,
                             prompt: String(repeating: "Read every file in my notes folder. ", count: 6),
                             elapsedMs: 0, spend: "$0.00", actions: [.stop])
    #expect(CorralCardController.measure(cardView(long)).height
            > CorralCardController.measure(cardView(short)).height)
}

@Test("TP-PLACE-06 Corral's panels act on the first click, whatever app is in front")
@MainActor
func panelsAcceptTheFirstClick() {
    #expect(CorralHostingView(rootView: EmptyView()).acceptsFirstMouse(for: nil))
}
