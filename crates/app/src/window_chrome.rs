//! Window chrome: on macOS the title bar is hidden and the content fills the window, so the
//! app's own header holds the traffic-light buttons. They are moved to sit centred in it.

/// Height of the app header (points).
pub const HEADER_H: f32 = 46.0;
/// Room the traffic lights take at the left of the header (points; 0 elsewhere).
pub const LIGHTS_W: f32 = if cfg!(target_os = "macos") { 84.0 } else { 0.0 };

/// Keep the window buttons vertically centred in the header, `x` points from the left edge.
/// Cheap; call every frame (AppKit puts the buttons back after resizes and full screen).
#[cfg(target_os = "macos")]
pub fn place_traffic_lights() {
    use objc2_app_kit::{NSApplication, NSWindowButton};
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect};
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let (x0, gap) = (18.0f64, 20.0f64);
    for w in app.windows().iter() {
        let buttons = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton].map(|b| w.standardWindowButton(b));
        let [Some(close), Some(_), Some(_)] = &buttons else { continue };
        // the buttons live in the title bar container view: give it the header's height
        // SAFETY: plain view hierarchy reads on the main thread.
        let Some(container) = (unsafe { close.superview().and_then(|v| v.superview()) }) else { continue };
        let bh = close.frame().size.height;
        let h = HEADER_H as f64;
        let wf = w.frame();
        let want = NSRect::new(NSPoint::new(0.0, wf.size.height - h), objc2_foundation::NSSize::new(wf.size.width, h));
        let cur = container.frame();
        if (cur.size.height - h).abs() > 0.5 || (cur.origin.y - want.origin.y).abs() > 0.5 || (cur.size.width - want.size.width).abs() > 0.5 {
            container.setFrame(want);
        }
        for (i, b) in buttons.iter().flatten().enumerate() {
            let o = NSPoint::new(x0 + gap * i as f64, (h - bh) / 2.0);
            let f = b.frame();
            if (f.origin.x - o.x).abs() > 0.5 || (f.origin.y - o.y).abs() > 0.5 {
                b.setFrameOrigin(o);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn place_traffic_lights() {}
