//! Dock / ⌘-Tab icon. diffvader is a bare executable, not an app bundle, so AppKit would
//! show the generic icon; setting `applicationIconImage` at runtime replaces it.

use objc2::ClassType;
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::{MainThreadMarker, NSData};

use crate::trace;

/// 512x512 PNG rendered by `assets/icon.py`.
static ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Installs the icon. Must run on the main thread; cheap (~1 ms), but deferred until after
/// the first frame so it never delays startup.
pub fn install() {
    let _s = trace::span("dock-icon");
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON_PNG);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    // Safety: plain AppKit setter called on the main thread with a valid image.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

/// Paints the NSWindow itself in the theme background. AppKit shows the window before our
/// first frame is presented (~90 ms on this machine), and its default gray would flash.
pub fn set_window_background(window: &winit::window::Window, color: u32) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let [r, g, b, _] = crate::theme::to_f64(color);
    // Safety: ns_view is a live NSView for as long as `window` exists; called on the main
    // thread, where winit created it.
    unsafe {
        let view: &objc2_app_kit::NSView = h.ns_view.cast().as_ref();
        if let Some(ns_window) = view.window() {
            let c = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
            ns_window.setBackgroundColor(Some(&c));
        }
    }
}
