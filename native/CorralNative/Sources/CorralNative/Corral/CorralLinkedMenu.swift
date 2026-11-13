// CorralLinkedMenu.swift
// The corral's right-click menu, second part: linked apps (ADR-005, docs/07 §8.3). Consumer Muse
// and your dot can't take tasks from Corral, so they show as drops that open the app. Rust
// decides what's shown and checks every choice; this only lists them and reports the click.

import SwiftUI

/// What the linked-apps part of the menu shows. Pure, so it's testable without a menu.
public struct CorralLinkedMenuModel: Equatable, Sendable {
    public struct Item: Equatable, Sendable {
        public let title: String
        public let checked: Bool
        public let intent: CorralIntent

        public init(title: String, checked: Bool, intent: CorralIntent) {
            self.title = title
            self.checked = checked
            self.intent = intent
        }
    }

    /// The dot looks OpenAI publishes, as Rust names them (`corral_adapters::linked::DotLook`).
    public static let looks: [(id: String, title: String)] = [
        ("ring", "Ring"), ("felipe", "Felipe"), ("alfred", "Alfred"), ("iggy", "Iggy"), ("todd", "Todd"),
    ]

    /// "Muse", checked when its drop shows; choosing it toggles.
    public let muse: Item
    /// One per look, checked on yours; choosing one shows your dot in it.
    public let dotLooks: [Item]
    /// "Hide your dot", when it shows.
    public let hideDot: Item?

    public init(links: CorralLinks) {
        muse = Item(title: "Muse", checked: links.muse,
                    intent: links.muse ? .hideLinked(id: "muse") : .showLinked(id: "muse", look: nil))
        dotLooks = Self.looks.map { look in
            Item(title: look.title, checked: links.dot == look.id,
                 intent: .showLinked(id: "dot", look: look.id))
        }
        hideDot = links.dot == nil ? nil
            : Item(title: "Hide your dot", checked: false, intent: .hideLinked(id: "dot"))
    }
}

/// The menu items.
public struct CorralLinkedMenu: View {
    let model: CorralLinkedMenuModel
    let onIntent: (CorralIntent) -> Void

    public init(model: CorralLinkedMenuModel, onIntent: @escaping (CorralIntent) -> Void) {
        self.model = model
        self.onIntent = onIntent
    }

    public var body: some View {
        Section("Linked apps") {
            item(model.muse)
            Menu("Your dot") {
                ForEach(model.dotLooks, id: \.title) { item($0) }
                if let hide = model.hideDot {
                    Divider()
                    item(hide)
                }
            }
        }
    }

    private func item(_ item: CorralLinkedMenuModel.Item) -> some View {
        Button {
            onIntent(item.intent)
        } label: {
            if item.checked {
                Label(item.title, systemImage: "checkmark")
            } else {
                Text(item.title)
            }
        }
    }
}
