// CorralSummonKey.swift
// The prompt bar's summon shortcut (docs/07 §6.1, TP-BAR-09): ⌃⌥Space by default, changeable from
// the corral's right-click menu and remembered. Registered with Carbon's RegisterEventHotKey,
// which works while another app is in front and needs no Accessibility permission.

import Carbon.HIToolbox
import Foundation

/// The shortcut that opens and closes the prompt bar. A display preference, like `CorralLevel`.
public enum CorralSummonKey: String, CaseIterable, Sendable {
    case controlOptionSpace
    case controlOptionP
    case controlShiftSpace
    case off

    public static let defaultsKey = "corral.summonKey"

    /// The menu wording.
    public var title: String {
        switch self {
        case .controlOptionSpace: "⌃⌥Space"
        case .controlOptionP: "⌃⌥P"
        case .controlShiftSpace: "⌃⇧Space"
        case .off: "Off"
        }
    }

    /// The key and modifiers Carbon registers; nil for Off.
    public var carbon: (keyCode: UInt32, modifiers: UInt32)? {
        switch self {
        case .controlOptionSpace: (keyCode: UInt32(kVK_Space), modifiers: UInt32(controlKey | optionKey))
        case .controlOptionP: (keyCode: UInt32(kVK_ANSI_P), modifiers: UInt32(controlKey | optionKey))
        case .controlShiftSpace: (keyCode: UInt32(kVK_Space), modifiers: UInt32(controlKey | shiftKey))
        case .off: nil
        }
    }

    /// The saved choice, or ⌃⌥Space.
    public static func load(from defaults: UserDefaults = .standard) -> CorralSummonKey {
        defaults.string(forKey: defaultsKey).flatMap(CorralSummonKey.init(rawValue:)) ?? .controlOptionSpace
    }

    public func save(to defaults: UserDefaults = .standard) {
        defaults.set(rawValue, forKey: Self.defaultsKey)
    }
}

/// The registered shortcut. One at a time: registering another replaces it.
@MainActor
public final class CorralHotkey {
    /// Called on the main thread when the shortcut is pressed.
    public var onFire: () -> Void
    private var ref: EventHotKeyRef?
    private var handler: EventHandlerRef?

    /// The one that fires: Carbon's callback is a C function and can't capture.
    private static weak var current: CorralHotkey?
    private static let signature: OSType = 0x434F_524C // "CORL"

    public init(onFire: @escaping () -> Void) {
        self.onFire = onFire
    }

    /// Registers `key` in place of the current one. False if another app holds it.
    @discardableResult
    public func register(_ key: CorralSummonKey) -> Bool {
        unregister()
        guard let keys = key.carbon else { return true }
        installHandler()
        var newRef: EventHotKeyRef?
        let id = EventHotKeyID(signature: Self.signature, id: 1)
        let status = RegisterEventHotKey(keys.keyCode, keys.modifiers, id, GetApplicationEventTarget(), 0, &newRef)
        guard status == noErr, let newRef else {
            NSLog("CorralNative: couldn't register the prompt bar shortcut %@ (%d)", key.title, status)
            return false
        }
        ref = newRef
        Self.current = self
        return true
    }

    public func unregister() {
        if let ref {
            // Nothing to do if it fails: the shortcut is going either way.
            _ = UnregisterEventHotKey(ref)
        }
        ref = nil
    }

    /// Stops listening (the app is quitting).
    public func stop() {
        unregister()
        if let handler {
            // Nothing to do if it fails: the app is quitting.
            _ = RemoveEventHandler(handler)
        }
        handler = nil
    }

    private func installHandler() {
        guard handler == nil else { return }
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        var installed: EventHandlerRef?
        let status = InstallEventHandler(GetApplicationEventTarget(), { _, _, _ in
            // Carbon delivers hot keys on the main run loop; hop explicitly all the same.
            DispatchQueue.main.async {
                MainActor.assumeIsolated { CorralHotkey.current?.onFire() }
            }
            return noErr
        }, 1, &spec, nil, &installed)
        if status == noErr {
            handler = installed
        } else {
            NSLog("CorralNative: couldn't listen for the prompt bar shortcut (%d)", status)
        }
    }
}
