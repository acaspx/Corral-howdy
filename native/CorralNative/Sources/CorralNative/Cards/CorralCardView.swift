// CorralCardView.swift
// A drop's card on paper (docs/07 §8.2, §8.4).
//
// Top to bottom: the agent's name in maroon small caps (and × when pinned), the task in New York
// serif, the status line, the answer or the error, the buttons, and the task field. Paper stays
// light in dark mode, like a sheet of paper on the desk. Everything shown comes from
// `CorralCardModel`; this view decides nothing.

import AppKit
import SwiftUI

public struct CorralCardView: View {
    public static let width: CGFloat = 340
    static let paper = Color(red: 0xFB / 255, green: 0xFA / 255, blue: 0xF8 / 255)
    static let maroon = Color(red: 0.48, green: 0.12, blue: 0.16)
    static let coral = Color(red: 0.91, green: 0.42, blue: 0.33)
    static let ink = Color(red: 0.11, green: 0.10, blue: 0.10)

    let model: CorralCardModel
    let pinned: Bool
    /// Bumped by the controller once the card's window is key, to put the cursor in the field.
    let focusRequest: Int
    let onButton: (CorralCardModel.Button) -> Void
    let onTask: (String) -> Void
    let onClose: () -> Void
    let onPointerInside: (Bool) -> Void

    @State private var text = ""
    @State private var guardrail = CorralInputGuard(shownAt: ProcessInfo.processInfo.systemUptime)
    @FocusState private var fieldFocused: Bool

    public init(model: CorralCardModel, pinned: Bool, focusRequest: Int = 0,
                onButton: @escaping (CorralCardModel.Button) -> Void,
                onTask: @escaping (String) -> Void,
                onClose: @escaping () -> Void,
                onPointerInside: @escaping (Bool) -> Void) {
        self.model = model
        self.pinned = pinned
        self.focusRequest = focusRequest
        self.onButton = onButton
        self.onTask = onTask
        self.onClose = onClose
        self.onPointerInside = onPointerInside
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text(model.label)
                    .font(.caption.weight(.semibold))
                    .tracking(1.4)
                    .foregroundStyle(Self.maroon)
                Spacer(minLength: 8)
                if pinned {
                    Button(action: onClose) {
                        Image(systemName: "xmark")
                            .font(.system(size: 11, weight: .bold))
                            .frame(width: 22, height: 22)
                            .contentShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Self.ink.opacity(0.5))
                    .accessibilityLabel("Close")
                    .help("Close (Esc)")
                }
            }
            Text(model.headline)
                .font(.system(.title3, design: .serif))
                .foregroundStyle(Self.ink)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
            Text(model.status)
                .font(.callout.monospacedDigit())
                .foregroundStyle(Self.ink.opacity(0.6))

            if let answer = model.answer {
                answerBox(answer)
            }
            if let error = model.error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.callout)
                    .foregroundStyle(Self.maroon)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !model.buttons.isEmpty {
                HStack(spacing: 8) {
                    ForEach(Array(model.buttons.enumerated()), id: \.element) { index, button in
                        cardButton(button, primary: index == 0 && button.isPrimary)
                    }
                    Spacer(minLength: 0)
                }
                .padding(.top, 2)
            }
            if let placeholder = model.fieldPlaceholder {
                taskField(placeholder)
            }
        }
        .padding(18)
        .frame(width: Self.width, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 23, style: .continuous).fill(Self.paper))
        .padding(7)
        .background(RoundedRectangle(cornerRadius: 30, style: .continuous).fill(.ultraThinMaterial))
        .environment(\.colorScheme, .light)
        .onHover(perform: onPointerInside)
        .onAppear {
            guardrail = CorralInputGuard(shownAt: ProcessInfo.processInfo.systemUptime)
            if pinned { fieldFocused = true }
        }
        .onChange(of: focusRequest) { _, _ in
            if pinned { fieldFocused = true }
        }
    }

    private func answerBox(_ answer: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("ANSWER FROM \(model.label)")
                .font(.caption2.weight(.semibold))
                .tracking(1.1)
                .foregroundStyle(Self.ink.opacity(0.5))
            ScrollView {
                Text(answer)
                    .font(.body)
                    .foregroundStyle(Self.ink)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(height: Self.answerHeight(answer))
            if model.answerTruncated {
                Text("Showing the first part of a long answer.")
                    .font(.caption)
                    .foregroundStyle(Self.ink.opacity(0.5))
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color.white))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(Self.ink.opacity(0.08)))
    }

    /// A rough height for the answer box: short answers fit, long ones scroll within 220 pt.
    static func answerHeight(_ answer: String) -> CGFloat {
        let lines = answer.split(separator: "\n", omittingEmptySubsequences: false)
            .reduce(0) { $0 + max(1, ($1.count + 41) / 42) }
        return min(220, CGFloat(lines) * 20 + 4)
    }

    private func cardButton(_ button: CorralCardModel.Button, primary: Bool) -> some View {
        Button {
            guard guardrail.accepts(at: ProcessInfo.processInfo.systemUptime) else { return }
            onButton(button)
        } label: {
            Text(button.title)
                .font(.callout.weight(.semibold))
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .foregroundStyle(primary ? Color.white : Self.ink)
                .background(Capsule().fill(primary ? Self.maroon : Self.ink.opacity(0.07)))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
    }

    private func taskField(_ placeholder: String) -> some View {
        HStack(spacing: 8) {
            TextField(placeholder, text: $text)
                .textFieldStyle(.plain)
                .font(.body)
                .foregroundStyle(Self.ink)
                .focused($fieldFocused)
                .onSubmit(send)
            Button(action: send) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 20))
                    .foregroundStyle(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                     ? Self.ink.opacity(0.2) : Self.coral)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Send task")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color.white))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous)
            .stroke(fieldFocused ? Self.coral.opacity(0.6) : Self.ink.opacity(0.1)))
    }

    private func send() {
        let task = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !task.isEmpty else { return }
        text = ""
        onTask(task)
    }
}
