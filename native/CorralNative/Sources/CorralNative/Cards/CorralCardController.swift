// CorralCardController.swift
// Shows a drop's card in its own panel above the corral (docs/07 §8.2).
//
// - Hover a drop: its card appears after 120 ms, without taking focus from the app in front.
// - Click a drop: its card is pinned and its window becomes key, so the cursor goes straight into
//   the task field. The app in front isn't deactivated and no other Corral window comes forward
//   (the panel is non-activating, like Spotlight).
// - ×, Esc or a click anywhere else closes a pinned card.
//
// Hover comes from one of two sources: SwiftUI's `onHover` on the drops and the card, or polling
// the pointer position 30 times a second. Spike S6 asks whether SwiftUI hover tracking works in a
// non-activating panel while another app is in front; `hoverSource` picks the one that does, and
// the playground logs both.

import AppKit
import SwiftUI

/// The card's window: borderless, non-activating, above app windows like the corral.
@MainActor
public final class CorralCardPanel: NSPanel {
    public init() {
        super.init(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        level = .floating
        hidesOnDeactivate = false
        isMovableByWindowBackground = false
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
    }

    /// The task field needs keyboard focus once the card is pinned.
    public override var canBecomeKey: Bool { true }
    public override var canBecomeMain: Bool { false }
}

public enum CorralHoverSource: String, CaseIterable, Sendable {
    case swiftUI = "SwiftUI hover"
    case pointerPolling = "Pointer polling"
}

@MainActor
public final class CorralCardController {
    /// Buttons and the task field send these, for the core to decide.
    public var onIntent: (CorralIntent) -> Void
    /// What happened, for the playground's log.
    public var onLog: (String) -> Void = { _ in }
    /// Which hover detector drives the card.
    public var hoverSource: CorralHoverSource = .swiftUI {
        didSet { updatePolling() }
    }

    private let corralPanel: CorralPanel
    private let panel: CorralCardPanel
    private let host: NSHostingView<AnyView>
    private var state = CorralHoverState()
    private var agents: [CorralAgentToken] = []
    private var cards: [String: CorralRunCard] = [:]
    private var deadlineTimer: Timer?
    private var pollTimer: Timer?
    private var monitors: [Any] = []
    private var swiftUIDrop: String?
    private var polledDrop: String?
    private var polledCard = false
    private var focusRequest = 0

    public init(corralPanel: CorralPanel, onIntent: @escaping (CorralIntent) -> Void) {
        self.corralPanel = corralPanel
        self.onIntent = onIntent
        panel = CorralCardPanel()
        host = NSHostingView(rootView: AnyView(EmptyView()))
        host.sizingOptions = []
        panel.contentView = host
        installMonitors()
        updatePolling()
    }

    /// Stops the monitors and timers (the app is quitting).
    public func stop() {
        for monitor in monitors { NSEvent.removeMonitor(monitor) }
        monitors.removeAll()
        deadlineTimer?.invalidate()
        pollTimer?.invalidate()
        panel.orderOut(nil)
    }

    /// The shown drops, most used first, and their run cards, as the core sent them.
    public func update(agents: [CorralAgentToken], cards: [String: CorralRunCard]) {
        self.agents = agents
        self.cards = cards
        if let shown = state.shown, !agents.contains(where: { $0.id == shown }) {
            state.dismiss()
        }
        render()
    }

    /// From `CorralGlassView`'s SwiftUI hover on drop `id`.
    public func swiftUIHover(drop id: String, inside: Bool) {
        if inside {
            swiftUIDrop = id
        } else if swiftUIDrop == id {
            swiftUIDrop = nil
        }
        if inside { onLog("hover (SwiftUI): \(name(of: id))") }
        guard hoverSource == .swiftUI else { return }
        state.pointer(overDrop: swiftUIDrop, at: now)
        changed()
    }

    /// A click on drop `id`: pin its card and put the cursor in its field.
    public func dropClicked(_ id: String) {
        state.click(drop: id)
        onLog("pinned \(name(of: id))")
        changed()
    }

    /// ×, Esc or a click elsewhere.
    public func dismiss() {
        guard state.shown != nil else { return }
        state.dismiss()
        changed()
    }

    // MARK: Drawing

    private var now: TimeInterval { ProcessInfo.processInfo.systemUptime }

    private func changed() {
        render()
        scheduleTick()
    }

    private func scheduleTick() {
        deadlineTimer?.invalidate()
        guard let deadline = state.deadline else { return }
        deadlineTimer = Timer.scheduledTimer(withTimeInterval: max(0, deadline - now) + 0.001,
                                             repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.state.tick(at: self.now)
                self.changed()
            }
        }
    }

    private func render() {
        guard let id = state.shown, let agent = agents.first(where: { $0.id == id }),
              let drop = dropFrame(id) else {
            if panel.isVisible {
                panel.orderOut(nil)
                onLog("card closed")
            }
            return
        }
        let model = CorralCardModel(agentName: agent.name, card: cards[id])
        host.rootView = AnyView(
            CorralCardView(
                model: model,
                pinned: state.pinned,
                focusRequest: focusRequest,
                onButton: { [weak self] button in
                    MainActor.assumeIsolated {
                        self?.send(CorralCardModel.intent(for: button, agentID: id))
                    }
                },
                onTask: { [weak self] text in
                    MainActor.assumeIsolated {
                        if let intent = CorralCardModel.task(text, agentID: id) { self?.send(intent) }
                    }
                },
                onClose: { [weak self] in
                    MainActor.assumeIsolated { self?.dismiss() }
                },
                onPointerInside: { [weak self] inside in
                    MainActor.assumeIsolated { self?.swiftUICardHover(inside) }
                }
            )
            .id(id)
        )
        let size = host.fittingSize
        panel.setFrame(frame(for: size, above: drop), display: true)
        if !panel.isVisible {
            panel.orderFrontRegardless()
            onLog("card opened: \(agent.name)\(state.pinned ? " (pinned)" : "")")
        }
        if state.pinned, !panel.isKeyWindow {
            panel.makeKey()
            focusRequest += 1
            // Re-render once the window is key so the field takes the cursor.
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.render() }
            }
        }
    }

    private func send(_ intent: CorralIntent) {
        onLog("card sent \(intent)")
        onIntent(intent)
    }

    /// Above the drop, centered on it and clamped to the screen, never covering the drop.
    private func frame(for size: CGSize, above drop: CGRect) -> CGRect {
        let screen = corralPanel.screen ?? CorralPanel.defaultScreen()
        let bounds = screen?.visibleFrame ?? .infinite
        var x = drop.midX - size.width / 2
        x = min(max(x, bounds.minX + 8), bounds.maxX - size.width - 8)
        let y = min(drop.maxY + 10, bounds.maxY - size.height - 8)
        return CGRect(x: x, y: y, width: size.width, height: size.height)
    }

    /// Drop `id`'s circle in screen coordinates, from the same layout the corral draws.
    private func dropFrame(_ id: String) -> CGRect? {
        let layout = CorralLayout.layout(rankedIDs: agents.map(\.id))
        guard let blob = layout.blobs.first(where: { $0.id == id }) else { return nil }
        let origin = corralPanel.frame.origin
        let height = corralPanel.frame.height
        let cx = origin.x + layout.pivot.x + blob.offset.x
        let cy = origin.y + height - (layout.pivot.y + blob.offset.y)
        return CGRect(x: cx - blob.radius, y: cy - blob.radius,
                      width: blob.radius * 2, height: blob.radius * 2)
    }

    private func name(of id: String) -> String {
        agents.first(where: { $0.id == id })?.name ?? id
    }

    // MARK: Hover sources

    private func swiftUICardHover(_ inside: Bool) {
        if inside { onLog("hover (SwiftUI): card") }
        guard hoverSource == .swiftUI else { return }
        state.pointer(overCard: inside, at: now)
        changed()
    }

    private func updatePolling() {
        pollTimer?.invalidate()
        pollTimer = nil
        polledDrop = nil
        polledCard = false
        guard hoverSource == .pointerPolling else { return }
        pollTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
    }

    private func poll() {
        let mouse = NSEvent.mouseLocation
        let drop = agents.first { agent in
            guard let frame = dropFrame(agent.id) else { return false }
            return hypot(mouse.x - frame.midX, mouse.y - frame.midY) <= frame.width / 2
        }?.id
        let onCard = panel.isVisible && panel.frame.contains(mouse)
        if drop != polledDrop {
            polledDrop = drop
            if let drop { onLog("hover (polling): \(name(of: drop))") }
            state.pointer(overDrop: drop, at: now)
            changed()
        }
        if onCard != polledCard {
            polledCard = onCard
            state.pointer(overCard: onCard, at: now)
            changed()
        }
    }

    // MARK: Closing on Esc and clicks elsewhere

    private func installMonitors() {
        // Clicks in other apps.
        if let global = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown],
                                                          handler: { [weak self] _ in
            MainActor.assumeIsolated { self?.clickedOutside(source: "another app") }
        }) {
            monitors.append(global)
        }
        // Esc, and clicks in Corral's own windows other than the card and the corral.
        if let local = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .keyDown],
                                                        handler: { [weak self] event in
            let isKey = event.type == .keyDown
            let isEsc = isKey && event.keyCode == 53
            let window = event.windowNumber
            let swallow = MainActor.assumeIsolated {
                self?.handleLocal(isKey: isKey, isEsc: isEsc, windowNumber: window) ?? false
            }
            return swallow ? nil : event
        }) {
            monitors.append(local)
        }
    }

    private func handleLocal(isKey: Bool, isEsc: Bool, windowNumber: Int) -> Bool {
        if isKey {
            guard isEsc, state.shown != nil else { return false }
            onLog("Esc")
            dismiss()
            return true
        }
        if windowNumber != panel.windowNumber && windowNumber != corralPanel.windowNumber {
            clickedOutside(source: "another Corral window")
        }
        return false
    }

    private func clickedOutside(source: String) {
        guard state.shown != nil else { return }
        onLog("click in \(source): closing")
        dismiss()
    }
}
