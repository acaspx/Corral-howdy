// CorralLevelMenu.swift
// The corral's right-click menu: where it sits among windows (docs/07 §4).

import SwiftUI

/// Menu items for choosing the corral's level, with a checkmark on the current one.
public struct CorralLevelMenu: View {
    let current: CorralLevel
    let onPick: (CorralLevel) -> Void

    public init(current: CorralLevel, onPick: @escaping (CorralLevel) -> Void) {
        self.current = current
        self.onPick = onPick
    }

    public var body: some View {
        Section("Show the corral") {
            ForEach(CorralLevel.allCases, id: \.self) { level in
                Button {
                    onPick(level)
                } label: {
                    if level == current {
                        Label(level.title, systemImage: "checkmark")
                    } else {
                        Text(level.title)
                    }
                }
            }
        }
    }
}
