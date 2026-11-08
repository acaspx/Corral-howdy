// swift-tools-version: 6.2
// CorralNative: macOS-only surfaces for Corral (the liquid-glass corral, hover cards,
// trace tree, actionable notifications, Touch ID). Linked into the Tauri app via swift-rs (Spike S1).
//
// Build with Xcode / xcodebuild, not bare `swift build`: SwiftPM's command-line driver
// does not run the Metal toolchain, so the .metallib would be missing.

import PackageDescription

let package = Package(
    name: "CorralNative",
    platforms: [.macOS(.v26)],   // Corral requires macOS 26 so system Liquid Glass is always available (ADR-011)
    products: [
        .library(name: "CorralNative", type: .static, targets: ["CorralNative"]),
    ],
    targets: [
        .target(
            name: "CorralNative",
            resources: [.process("Corral/CorralGlass.metal")]
        ),
        .testTarget(name: "CorralNativeTests", dependencies: ["CorralNative"]),
    ]
)
