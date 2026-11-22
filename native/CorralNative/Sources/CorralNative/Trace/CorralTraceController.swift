// CorralTraceController.swift
// The trace tree's window (docs/07 §8.3): its own borderless panel at the corral's level, rising
// from the corral's top (or above the open prompt bar), click-through for now. Shown only while
// something runs; never animated. Clicks on its cards come with TP-TRACE-05.

import AppKit
import SwiftUI

@MainActor
final class CorralTracePanel: NSPanel {
    init() {
        super.init(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        ignoresMouseEvents = true
        hidesOnDeactivate = false
        animationBehavior = .none
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
public final class CorralTraceController {
    private let corralPanel: CorralPanel
    private let model = CorralTraceModel()
    private let panel = CorralTracePanel()
    private let host: NSHostingView<AnyView>

    public init(corralPanel: CorralPanel) {
        self.corralPanel = corralPanel
        host = CorralHostingView(rootView: AnyView(EmptyView()))
        host.sizingOptions = []
        panel.contentView = host
        host.rootView = AnyView(CorralTraceRoot(model: model))
    }

    /// Shows `trace` over the corral's drops (`rankedIDs`, in the corral's order). `barFrame`: the
    /// open prompt bar's window, which the tree then sits above.
    public func update(trace: CorralTrace, rankedIDs: [String], barFrame: CGRect?) {
        guard CorralTraceLayout.shows(trace) else {
            panel.orderOut(nil)
            return
        }
        let corral = corralPanel.frame
        let screen = corralPanel.screen ?? CorralPanel.defaultScreen()
        let visible = screen?.visibleFrame ?? CGRect(x: 0, y: 0, width: 1440, height: 900)
        let above = barFrame.map { $0.maxY + CorralTraceLayout.margin } ?? corral.maxY
        let columns = trace.columns.count
        model.trace = trace
        model.wires = CorralTraceLayout.showsWires(barOpen: barFrame != nil)
        model.width = CorralTraceLayout.frame(columns: columns, height: 0, corral: corral, above: above,
                                              visible: visible).width
        let height = CorralCardController.measure(AnyView(CorralTraceRoot(model: model))).height
        let frame = CorralTraceLayout.frame(columns: columns, height: height, corral: corral, above: above,
                                            visible: visible)
        let drops = CorralTraceLayout.dropXs(rankedIDs: rankedIDs, corralMinX: corral.minX)
        // IDs arrive deduped; keep the first rather than trap if one ever repeats.
        model.anchors = Dictionary(trace.columns.map { column in
            (column.id, CorralTraceLayout.anchorX(for: column.id, drops: drops, corral: corral) - frame.minX)
        }, uniquingKeysWith: { first, _ in first })
        panel.level = corralPanel.level
        panel.setFrame(frame, display: true)
        if !panel.isVisible {
            panel.orderFrontRegardless()
        }
    }

    /// Hides the tree (the app is quitting).
    public func stop() {
        panel.orderOut(nil)
    }
}
