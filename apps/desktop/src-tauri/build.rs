//! Generates Tauri's context (config, capabilities, icons) at compile time. With the `native`
//! feature (Spike S1) it also builds the CorralNative Swift package and links it.
fn main() {
    #[cfg(feature = "native")]
    link_corral_native();
    tauri_build::build()
}

#[cfg(feature = "native")]
fn link_corral_native() {
    // Build scripts run on the host; the target is what matters for linking.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // Must match `platforms: [.macOS(.v26)]` in Package.swift and `minimumSystemVersion`.
    swift_rs::SwiftLinker::new("26.0")
        .with_package("CorralNative", "../../../native/CorralNative")
        .link();
    // The Swift runtime ships in the OS on macOS 26; resolve it there.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    println!("cargo:rerun-if-changed=../../../native/CorralNative/Sources");
    println!("cargo:rerun-if-changed=../../../native/CorralNative/Package.swift");
}
