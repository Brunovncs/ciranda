// The icon is resource 1, which GPUI uses for the window and Windows for the file. The version
// resource names the program in Task Manager and the file's properties, whatever the file is
// called (an update from Harmony keeps `harmony.exe`). No manifest here: GPUI embeds its own.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/ciranda.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let version = [
            format!("VERSION_MAJOR={}", env!("CARGO_PKG_VERSION_MAJOR")),
            format!("VERSION_MINOR={}", env!("CARGO_PKG_VERSION_MINOR")),
            format!("VERSION_PATCH={}", env!("CARGO_PKG_VERSION_PATCH")),
        ];
        embed_resource::compile("assets/ciranda.rc", &version).manifest_optional().unwrap();
    }
    // libwebrtc's screen capture asks the desktop portal over D-Bus through GLib, whose headers
    // webrtc-sys compiles against but whose libraries it leaves to the program. Every desktop
    // has them, so they are linked from the system and not carried in the AppImage.
    // libwebrtc's Objective-C categories (on NSString, among others) live in its static library;
    // without -ObjC the linker leaves them out, and the program dies as it loads.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        for lib in ["gio-2.0", "gobject-2.0", "glib-2.0"] {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
}
