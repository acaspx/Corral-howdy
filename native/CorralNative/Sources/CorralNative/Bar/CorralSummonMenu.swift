// CorralSummonMenu.swift
// The corral's right-click menu: which shortcut opens the prompt bar (docs/07 §6.1, TP-BAR-09).

import SwiftUI

public struct CorralSummonMenu: View {
    let current: CorralSummonKey
    /// The shortcut is held by another app.
    let taken: Bool
    let onPick: (CorralSummonKey) -> Void

    public init(current: CorralSummonKey, taken: Bool, onPick: @escaping (CorralSummonKey) -> Void) {
        self.current = current
        self.taken = taken
        self.onPick = onPick
    }

    public var body: some View {
        Section("Prompt bar shortcut") {
            ForEach(CorralSummonKey.allCases, id: \.self) { key in
                Button {
                    onPick(key)
                } label: {
                    if key == current {
                        Label(key.title, systemImage: "checkmark")
                    } else {
                        Text(key.title)
                    }
                }
            }
            if taken {
                Text("\(current.title) is taken by another app. Pick another.")
            }
        }
    }
}
