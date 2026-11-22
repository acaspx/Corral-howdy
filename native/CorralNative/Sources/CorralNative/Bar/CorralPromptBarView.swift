// CorralPromptBarView.swift
// The prompt bar (docs/07 §6.1): one capsule row of dark-tinted glass, 44 pt tall, with who it
// goes to, the field and the send button; the @ suggestions above it on paper; the error line
// under it. Rust decides the chips, placeholder, button, error line and rows; this only draws
// them, and the bar's state (`CorralPromptBarState`) decides what keys do.

import SwiftUI

/// The bar's state, for SwiftUI to observe.
@MainActor
public final class CorralPromptBarModel: ObservableObject {
    @Published public internal(set) var state = CorralPromptBarState()
    /// Bumped to put the cursor in the field.
    @Published var focusRequest = 0
    /// The bar's width, from the corral's.
    @Published var width: CGFloat = 480

    public init() {}
}

struct CorralPromptBarRoot: View {
    @ObservedObject var model: CorralPromptBarModel
    @ObservedObject var answer: CorralPromptBar
    var onField: (String, Int) -> Void
    var onKey: (CorralPromptBarState.Key) -> Bool
    var onSend: (Bool) -> Void
    var onTake: (Int) -> Void

    static let chipFill = Color(red: 0xEA / 255, green: 0xF1 / 255, blue: 0xFE / 255)
    static let chipText = Color(red: 0x3B / 255, green: 0x6F / 255, blue: 0xD8 / 255)
    static let inkFaint = Color(red: 0x9B / 255, green: 0x95 / 255, blue: 0x8E / 255)

    var body: some View {
        let view = answer.view
        let rows = view?.suggestions ?? []
        VStack(alignment: .leading, spacing: 6) {
            if model.state.listShown(rows.count) {
                suggestions(rows)
            }
            bar(view)
            line(view)
        }
        .frame(width: model.width, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
        .transaction { $0.animation = nil }   // never animated (docs/07 §7.4)
    }

    // MARK: The row

    private func bar(_ view: CorralPromptBarView?) -> some View {
        HStack(spacing: 8) {
            chips(view?.chips ?? [])
            CorralPromptField(
                text: model.state.text,
                cursor: model.state.cursor,
                placeholder: view?.placeholder ?? "",
                enabled: !model.state.sending,
                focusRequest: model.focusRequest,
                onChange: onField,
                onKey: onKey)
                .frame(maxWidth: .infinity)
            sendButton(view?.button)
        }
        .padding(.horizontal, 14)
        .frame(height: 44)
        .glassEffect(.regular.tint(Color.black.opacity(0.45)), in: .capsule)
    }

    @ViewBuilder
    private func chips(_ chips: [String]) -> some View {
        if !chips.isEmpty {
            HStack(spacing: 4) {
                ForEach(Array(chips.enumerated()), id: \.offset) { _, chip in
                    Text(chip)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Self.chipText)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(RoundedRectangle(cornerRadius: 6).fill(Self.chipFill))
                        .lineLimit(1)
                }
            }
            .fixedSize()
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Sends to \(Self.andList(chips))")
        }
    }

    @ViewBuilder
    private func sendButton(_ button: CorralPromptBarView.Button?) -> some View {
        if let button {
            Button(button.label) { onSend(button.all) }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
                .disabled(!button.enabled || model.state.sending)
                .accessibilityLabel(button.label)
                .fixedSize()
        }
    }

    // MARK: The error line

    /// What the line under the bar says: a send's note or refusal until the next edit, otherwise
    /// the core's error line.
    struct Line: Equatable {
        var glyph: String
        var text: String
        var isError: Bool
    }

    nonisolated static func lineContent(message: CorralPromptBarState.Message?, error: String?) -> Line? {
        switch message {
        case .note(let note)?: return Line(glyph: "info.circle", text: note, isError: false)
        case .refused(let why)?: return Line(glyph: "exclamationmark.circle", text: why, isError: true)
        case nil: return error.map { Line(glyph: "exclamationmark.circle", text: $0, isError: true) }
        }
    }

    @ViewBuilder
    private func line(_ view: CorralPromptBarView?) -> some View {
        if let shown = Self.lineContent(message: model.state.message, error: view?.error) {
            Label(shown.text, systemImage: shown.glyph)
                .font(.system(size: 12))
                .foregroundStyle(shown.isError ? CorralCardView.maroon : CorralCardView.ink)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .background(Capsule().fill(CorralCardView.paper))
                .padding(.leading, 14)
                .accessibilityLabel(shown.isError ? "Error: \(shown.text)" : shown.text)
        }
    }

    // MARK: The @ suggestions

    private func suggestions(_ rows: [CorralPromptBarView.Suggestion]) -> some View {
        let highlighted = min(model.state.highlighted, rows.count - 1)
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                Button { onTake(index) } label: {
                    HStack(spacing: 8) {
                        logo(row)
                        Text(row.name)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(CorralCardView.ink)
                        if let status = Self.statusText(row.status) {
                            Text(status).font(.system(size: 12)).foregroundStyle(CorralCardView.maroon)
                        }
                        Spacer(minLength: 8)
                        if !row.shown {
                            Text("hidden").font(.system(size: 12)).foregroundStyle(Self.inkFaint)
                        }
                    }
                    .padding(.horizontal, 12)
                    .frame(height: 30)
                    .contentShape(Rectangle())
                    .background(index == highlighted ? Self.chipFill : Color.clear)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Self.rowLabel(row))
                .accessibilityAddTraits(index == highlighted ? .isSelected : [])
            }
        }
        .padding(.vertical, 4)
        .frame(width: min(280, model.width), alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 16).fill(CorralCardView.paper))
        .shadow(color: .black.opacity(0.12), radius: 10, y: 4)
        .padding(.leading, 14)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(rows.count == 1 ? "1 suggestion" : "\(rows.count) suggestions")
    }

    /// The agent's logo, 18 pt, or its initial when there's none.
    @ViewBuilder
    private func logo(_ row: CorralPromptBarView.Suggestion) -> some View {
        if let path = row.logoPath, let image = CorralLogoCache.shared.image(at: path) {
            Image(decorative: image, scale: 1)
                .resizable()
                .interpolation(.high)
                .frame(width: 18, height: 18)
                .clipShape(RoundedRectangle(cornerRadius: 4))
        } else {
            Text(String(row.name.prefix(1)))
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Self.chipText)
                .frame(width: 18, height: 18)
                .background(RoundedRectangle(cornerRadius: 4).fill(Self.chipFill))
        }
    }

    nonisolated static func statusText(_ status: CorralPromptBarView.Status) -> String? {
        switch status {
        case .ready: nil
        case .working: "Working"
        case .needsYou: "Needs you"
        }
    }

    nonisolated static func rowLabel(_ row: CorralPromptBarView.Suggestion) -> String {
        var label = row.name
        if let status = statusText(row.status) { label += ", \(status)" }
        if !row.shown { label += ", hidden" }
        return label
    }

    /// "Grok", "Grok and Claude", "Grok, Claude and GPT".
    nonisolated static func andList(_ names: [String]) -> String {
        guard let last = names.last else { return "" }
        guard names.count > 1 else { return last }
        return names.dropLast().joined(separator: ", ") + " and " + last
    }
}
