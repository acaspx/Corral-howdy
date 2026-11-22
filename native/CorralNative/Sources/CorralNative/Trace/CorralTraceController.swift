// CorralTraceController.swift
// The trace tree's window (docs/07 §8.3): its own borderless panel at the corral's level, rising
// from the corral's top (or above the open prompt bar). Shown only while something runs; never
// animated. A task card opens its run in the trace viewer (Rust finds the run); a subtask stack
// fans out (TP-TRACE-05). The panel never takes focus, and clicks between its cards reach what's
// underneath.

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

    /// The last tree shown, to redraw it when a stack fans out.
    private var shown: (trace: CorralTrace, rankedIDs: [String], barFrame: CGRect?)?

    /// `onIntent`: a task card's click, for Rust.
    public init(corralPanel: CorralPanel, onIntent: @escaping (CorralIntent) -> Void) {
        self.corralPanel = corralPanel
        host = CorralHostingView(rootView: AnyView(EmptyView()))
        host.sizingOptions = []
        panel.contentView = host
        host.rootView = AnyView(CorralTraceRoot(model: model))
        model.onOpen = { id in onIntent(.openTrace(id: id)) }
        model.onFan = { [weak self] id in
            MainActor.assumeIsolated { self?.fan(id) }
        }
    }

    private func fan(_ id: String) {
        model.fanned = CorralTraceLayout.toggled(model.fanned, id)
        if let shown { update(trace: shown.trace, rankedIDs: shown.rankedIDs, barFrame: shown.barFrame) }
    }

    /// Shows `trace` over the corral's drops (`rankedIDs`, in the corral's order). `barFrame`: the
    /// open prompt bar's window, which the tree then sits above.
    public func update(trace: CorralTrace, rankedIDs: [String], barFrame: CGRect?) {
        shown = (trace, rankedIDs, barFrame)
        model.fanned = CorralTraceLayout.kept(model.fanned, columns: trace.columns.map(\.id))
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
