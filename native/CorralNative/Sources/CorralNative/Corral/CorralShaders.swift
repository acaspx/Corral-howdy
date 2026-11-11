// CorralShaders.swift
// Finds the compiled corral shader without ever trapping (Spike S1 finding).
//
// `ShaderLibrary.bundle(.module)` goes through SwiftPM's generated `Bundle.module`, which calls
// fatalError when the resource bundle isn't where SwiftPM expects it. Linked into the Tauri app,
// the bundle is somewhere else or missing, and that trap would take the Rust core down with it
// (same process). So the shader is looked up in the known places, and if it isn't found the corral
// draws plain system glass instead.

import SwiftUI

enum CorralShaders {
    static let resourceBundleName = "CorralNative_CorralNative.bundle"

    /// The corral's shader library, or nil when no compiled shader was found (plain glass is drawn).
    @MainActor static let library: ShaderLibrary? = find()

    static func find(in main: Bundle = .main) -> ShaderLibrary? {
        // 1. A metallib copied into the app's Resources (the packaged .app), or next to a dev binary.
        if let url = main.url(forResource: "CorralGlass", withExtension: "metallib") {
            return ShaderLibrary(url: url)
        }
        // 2. SwiftPM's resource bundle, compiled by Xcode (xcodebuild products, the playground).
        let bases = [main.resourceURL, main.bundleURL, main.executableURL?.deletingLastPathComponent()]
        for base in bases.compactMap({ $0 }) {
            if let bundle = Bundle(url: base.appendingPathComponent(resourceBundleName)),
               bundle.url(forResource: "default", withExtension: "metallib") != nil {
                return .bundle(bundle)
            }
        }
        return nil
    }
}
