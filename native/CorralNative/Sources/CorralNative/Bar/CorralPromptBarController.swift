// CorralPromptBarController.swift
// The prompt bar's window (docs/07 §6.1): its own non-activating panel 8 pt above the corral,
// 480 to 640 pt wide, made key without bringing Corral's other windows forward, shown and hidden
// with no animation. Opening it closes a pinned card. Esc, the field's keys and a click elsewhere
// go through `CorralPromptBarState`; each edit and send goes to Rust, and Rust's answer comes
// back through `received`.

import AppKit
import SwiftUI

/// The bar's window: borderless, non-activating, above app windows, never animated.
@MainActor
public final class CorralPromptBarPanel: NSPanel {
    public init() {
        super.init(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        level = .floating
        hidesOnDeactivate = false
        isMovableByWindowBackground = false
        animationBehavior = .none
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
    }

    /// The field needs keyboard focus.
    public override var canBecomeKey: Bool { true }
    public override var canBecomeMain: Bool { false }
}

@MainActor
public final class CorralPromptBarController {
    public nonisolated static let gap: CGFloat = 8
    public nonisolated static let minWidth: CGFloat = 480
    public nonisolated static let maxWidth: CGFloat = 640
    nonisolated static let margin: CGFloat = 8

    public let model = CorralPromptBarModel()
    /// Edits and sends, for Rust.
    public var onIntent: (CorralIntent) -> Void
    /// The bar is opening: close a pinned card, so there's one place to type.
    public var onOpen: () -> Void
    /// ↓ with no suggestion list: the corral takes keyboard focus (TP-A11Y-02).
    public var onToCorral: () -> Void = {}
    /// The bar was shown (true) or hidden (false): the corral comes up with it.
    public var onShown: (Bool) -> Void = { _ in }

    private let corralPanel: CorralPanel
    private let answer: CorralPromptBar
    private let panel = CorralPromptBarPanel()
    private let host: NSHostingView<AnyView>
    private var selection: [String] = []
    private var monitors: [Any] = []

    public init(corralPanel: CorralPanel, answer: CorralPromptBar,
                onIntent: @escaping (CorralIntent) -> Void, onOpen: @escaping () -> Void) {
        self.corralPanel = corralPanel
        self.answer = answer
        self.onIntent = onIntent
        self.onOpen = onOpen
        host = CorralHostingView(rootView: AnyView(EmptyView()))
        host.sizingOptions = []
        panel.contentView = host
        host.rootView = AnyView(root)
        installMonitors()
    }

    public var isOpen: Bool { model.state.isOpen }

    /// The bar's window while it's shown, for the trace tree to sit above.
    public var shownFrame: CGRect? { panel.isVisible ? panel.frame : nil }

    /// Opens the bar; with `name`, that agent is named first (a drop dragged up).
    public func open(naming name: String? = nil) {
        onOpen()
        act(model.state.open(naming: name))
        model.focusRequest += 1
        render()
    }

    /// The summon shortcut: opens the bar, or closes it if it's open.
    public func summon() {
        if !model.state.isOpen { onOpen() }
        act(model.state.summoned())
        if model.state.isOpen { model.focusRequest += 1 }
        render()
    }

    /// Closes the bar, keeping the text.
    public func close() {
        guard model.state.isOpen else { return }
        model.state.close()
        render()
    }

    /// The drops ⌘-clicked, in the corral's order.
    public func setSelection(_ ids: [String]) {
        guard ids != selection else { return }
        selection = ids
        if model.state.isOpen { act(.edited) }
    }

    /// Rust's answer, already taken by `CorralPromptBar` as the latest. A send's outcome may close
    /// the bar or clear its text.
    public func received(_ outcome: CorralPromptBarOutcome?) {
        model.state.answered(outcome)
        render()
    }

    /// Stops the monitors (the app is quitting).
    public func stop() {
        for monitor in monitors { NSEvent.removeMonitor(monitor) }
        monitors.removeAll()
        panel.orderOut(nil)
    }

    // MARK: The field

    private var root: some View {
        CorralPromptBarRoot(
            model: model, answer: answer,
            onField: { [weak self] text, cursor in
                MainActor.assumeIsolated { self?.fieldChanged(text: text, cursor: cursor) }
            },
            onKey: { [weak self] key in
                MainActor.assumeIsolated { self?.key(key) ?? false }
            },
            onSend: { [weak self] all in
                MainActor.assumeIsolated { self?.sendButton(all: all) }
            },
            onTake: { [weak self] index in
                MainActor.assumeIsolated { self?.take(index) }
            })
    }

    private func fieldChanged(text: String, cursor: Int) {
        act(model.state.edit(text: text, cursor: cursor))
    }

    private func key(_ key: CorralPromptBarState.Key) -> Bool {
        let action = model.state.key(key, suggestions: answer.view?.suggestions ?? [])
        act(action)
        return action != .pass
    }

    private func sendButton(all: Bool) {
        act(model.state.key(all ? .commandEnter : .enter, suggestions: []))
    }

    /// A click on a suggestion row.
    private func take(_ index: Int) {
        act(model.state.choose(index, suggestions: answer.view?.suggestions ?? []))
        model.focusRequest += 1
    }

    private func act(_ action: CorralPromptBarState.Action) {
        switch action {
        case .none, .pass:
            break
        case .edited:
            onIntent(answer.edited(text: model.state.text, utf16Cursor: model.state.cursor, selection: selection))
        case .send(let all):
            onIntent(answer.send(text: model.state.text, selection: selection, all: all))
        case .close:
            render()
        case .toCorral:
            onToCorral()
        }
    }

    // MARK: The window

    private func render() {
        guard model.state.isOpen else {
            if panel.isVisible {
                panel.orderOut(nil)
                onShown(false)
            }
            return
        }
        let screen = corralPanel.screen ?? CorralPanel.defaultScreen()
        let visible = screen?.visibleFrame ?? CGRect(x: 0, y: 0, width: 1440, height: 900)
        model.width = Self.width(corral: corralPanel.frame.width, visible: visible)
        let size = CorralCardController.measure(AnyView(root))
        panel.setFrame(Self.frame(height: size.height, corral: corralPanel.frame, visible: visible), display: true)
        if !panel.isVisible {
            onShown(true)
            panel.orderFrontRegardless()
        }
        if !panel.isKeyWindow {
            panel.makeKey()
        }
    }

    nonisolated static func width(corral: CGFloat, visible: CGRect) -> CGFloat {
        min(max(corral, minWidth), maxWidth, visible.width - 2 * margin)
    }

    /// 8 pt above the corral, centered on it, kept on screen.
    nonisolated static func frame(height: CGFloat, corral: CGRect, visible: CGRect) -> CGRect {
        let width = width(corral: corral.width, visible: visible)
        var x = corral.midX - width / 2
        x = min(max(x, visible.minX + margin), visible.maxX - width - margin)
        let y = min(corral.maxY + gap, visible.maxY - height - margin)
        return CGRect(x: x, y: y, width: width, height: height)
    }

    // MARK: Closing on clicks elsewhere

    private func installMonitors() {
        if let global = NSEvent.addGlobalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown],
                                                          handler: { [weak self] _ in
            MainActor.assumeIsolated { self?.close() }
        }) {
            monitors.append(global)
        }
        // Clicks in Corral's other windows close it; clicks in the corral don't, so ⌘-click can
        // change the selection while the bar is open.
        if let local = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown],
                                                        handler: { [weak self] event in
            let window = event.windowNumber
            MainActor.assumeIsolated {
                guard let self, window != self.panel.windowNumber, window != self.corralPanel.windowNumber else { return }
                self.close()
            }
            return event
        }) {
            monitors.append(local)
        }
    }
}
