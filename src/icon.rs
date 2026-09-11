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
