// CorralPromptField.swift
// The prompt bar's text field: an AppKit field, because its field editor reports Return, Esc,
// ↑↓, Tab and ⌘⌫ as commands before handling them, and its cursor in UTF-16, which is what the
// bar needs (`CorralPromptBarState`). SwiftUI's TextField lets the field editor take those keys first.

import AppKit
import SwiftUI

struct CorralPromptField: NSViewRepresentable {
    /// What the field shows: the bar's state decides, the field only reports.
    var text: String
    var cursor: Int
    var placeholder: String
    var enabled: Bool
    /// Bumped when the bar opens, to put the cursor in the field.
    var focusRequest: Int
    var onChange: (String, Int) -> Void
    /// A key for the bar. `true`: the bar used it.
    var onKey: (CorralPromptBarState.Key) -> Bool

    func makeNSView(context: Context) -> NSTextField {
        let field = NSTextField(string: text)
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = .systemFont(ofSize: 15)
        field.textColor = .white
        field.lineBreakMode = .byTruncatingHead
        field.cell?.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.delegate = context.coordinator
        field.setAccessibilityLabel("Prompt bar")
        context.coordinator.field = field
        context.coordinator.observeSelection()
        return field
    }

    func updateNSView(_ field: NSTextField, context: Context) {
        context.coordinator.parent = self
        field.placeholderAttributedString = NSAttributedString(
            string: placeholder,
            attributes: [.foregroundColor: NSColor.white.withAlphaComponent(0.55), .font: NSFont.systemFont(ofSize: 15)])
        field.isEnabled = enabled
        if field.stringValue != text {
            field.stringValue = text
        }
        let editor = field.currentEditor() as? NSTextView
        if let editor, editor.selectedRange().location != cursor || editor.selectedRange().length != 0 {
            let at = min(max(0, cursor), (text as NSString).length)
            editor.setSelectedRange(NSRange(location: at, length: 0))
        }
        if context.coordinator.focused != focusRequest, let window = field.window {
            context.coordinator.focused = focusRequest
            window.makeFirstResponder(field)
            (field.currentEditor() as? NSTextView)?.setSelectedRange(NSRange(location: min(cursor, (text as NSString).length), length: 0))
        }
    }

    static func dismantleNSView(_ field: NSTextField, coordinator: Coordinator) {
        coordinator.stopObserving()
    }

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    @MainActor
    final class Coordinator: NSObject, NSTextFieldDelegate {
        var parent: CorralPromptField
        weak var field: NSTextField?
        var focused = -1
        private var observer: NSObjectProtocol?

        init(parent: CorralPromptField) { self.parent = parent }

        func controlTextDidChange(_ note: Notification) { report() }

        /// The cursor moved without the text changing (arrows, a click).
        func observeSelection() {
            observer = NotificationCenter.default.addObserver(
                forName: NSTextView.didChangeSelectionNotification, object: nil, queue: .main) { [weak self] note in
                // Only the object's identity crosses into the main actor: it's Sendable, the object isn't.
                let editor = (note.object as AnyObject?).map(ObjectIdentifier.init)
                MainActor.assumeIsolated {
                    guard let self, let field = self.field, let current = field.currentEditor(),
                          editor == ObjectIdentifier(current) else { return }
                    self.report()
                }
            }
        }

        func stopObserving() {
            if let observer { NotificationCenter.default.removeObserver(observer) }
            observer = nil
        }

        private func report() {
            guard let field, let editor = field.currentEditor() as? NSTextView else { return }
            let range = editor.selectedRange()
            parent.onChange(field.stringValue, range.location + range.length)
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            let command = NSApp.currentEvent?.modifierFlags.contains(.command) ?? false
            let key: CorralPromptBarState.Key
            switch selector {
            case #selector(NSResponder.moveUp(_:)): key = .up
            case #selector(NSResponder.moveDown(_:)): key = .down
            case #selector(NSResponder.insertNewline(_:)): key = command ? .commandEnter : .enter
            case #selector(NSResponder.insertTab(_:)): key = .tab
            case #selector(NSResponder.cancelOperation(_:)): key = .escape
            case #selector(NSResponder.deleteToBeginningOfLine(_:)): key = .clear
            case Selector(("noop:")):
                // ⌘Return has no binding in a text field, so it arrives as noop.
                guard command, NSApp.currentEvent?.keyCode == 36 else { return false }
                key = .commandEnter
            default:
                return false
            }
            return parent.onKey(key)
        }
    }
}
