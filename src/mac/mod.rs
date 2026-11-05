//! macOS backend: the same modules as the X11 one (capture, desktop, input, audio,
//! screen) on Apple's APIs, so the rest of voxl (encoder, flow control, server, web
//! client) runs unchanged.
//!
//! - capture: ScreenCaptureKit, NV12 frames with dirty rectangles
//! - input: CGEvent (pixel scrolling, keys resolved against the current layout)
//! - desktop: cursor images from NSCursor, clipboard sync through NSPasteboard
//! - audio: system audio from ScreenCaptureKit
//! - screen: dynamic resolution by switching display modes
//! - mic: browser microphone played into a loopback output device
//!
//! With `--app` (an app's name or bundle ID, such as `Simulator`), one window of that
//! app is streamed instead of the whole display: it is captured even while other
//! windows cover it, input is mapped into it, and the app is brought to the front
//! when a viewer clicks or types.

pub mod audio;
pub mod capture;
pub mod desktop;
pub mod ffi;
pub mod input;
pub mod mic;
pub mod screen;
pub mod sim;
pub mod stream;

use ffi::*;
use parking_lot::Mutex;
use std::collections::HashMap;

/// The captured display (or window) and how its points map to the desktop pixels
/// voxl streams.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub display: u32,
    /// Display (or, with --app, window) bounds in global points.
    pub bounds: CGRect,
    /// Streamed size in pixels.
    pub width: u32,
    /// With --app: the streamed window's process, raised before input reaches it.
    pub pid: Option<i32>,
}

impl Geometry {
    /// Desktop pixels per point.
    pub fn scale(&self) -> f64 {
        self.width as f64 / self.bounds.size.width.max(1.0)
    }
}

/// Set by the capturer whenever it (re)starts; read by input to map coordinates.
pub static GEOMETRY: Mutex<Option<Geometry>> = Mutex::new(None);

/// `--app` and `--window-title`: which application window to stream.
#[derive(Clone, Debug)]
pub struct AppTarget {
    /// App name or bundle ID, matched case-insensitively (exactly, else as a substring).
    pub query: String,
    /// Only windows whose title contains this (case-insensitive).
    pub title: Option<String>,
    /// For the iOS Simulator: stream just the device screen, not the window around it
    /// (`--sim-screen`, default true).
    pub screen_only: bool,
}

struct Settings {
    display: u32,
    app: Option<AppTarget>,
    /// Fixed pixels per point (--capture-scale), or None for automatic.
    scale: Option<f64>,
    max_pixels: f64,
    fps: u32,
}

static SETTINGS: Mutex<Settings> = Mutex::new(Settings {
    display: 0,
    app: None,
    scale: None,
    max_pixels: 2560.0 * 1600.0,
    fps: 60,
});

/// Reads the macOS options and checks the privacy permissions voxl needs.
pub fn init(a: &HashMap<String, String>) {
    let display = pick_display(a.get("display").map(|s| s.as_str()));
    let app = a
        .get("app")
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty())
        .map(|query| AppTarget {
            query,
            title: a
                .get("window-title")
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty()),
            screen_only: !matches!(
                a.get("sim-screen").map(|v| v.as_str()),
                Some("false" | "0" | "no" | "off")
            ),
        });
    {
        let mut s = SETTINGS.lock();
        s.display = display;
        s.app = app.clone();
        s.scale = a
            .get("capture-scale")
            .and_then(|v| v.parse().ok())
            .filter(|&v: &f64| v > 0.0);
        if let Some(px) = a.get("max-capture-pixels").and_then(|v| v.parse().ok()) {
            s.max_pixels = px;
        }
        // One app's window is small, so stream it at up to 120 fps by default: the
        // Simulator renders at ProMotion rates, and capturing a 60 Hz source at exactly
        // 60 Hz drops every frame that lands just before the interval.
        if app.is_some() {
            s.fps = 120;
        }
        // Selkies-style ranges ("60-60"): the upper bound.
        if let Some(f) = a
            .get("framerate")
            .and_then(|v| v.rsplit('-').next()?.trim().parse::<f64>().ok())
        {
            s.fps = (f as u32).clamp(1, 240);
        }
    }
    unsafe {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
            log::error!(
                "Screen Recording permission is missing: allow voxl (or the terminal running it) \
                 in System Settings > Privacy & Security > Screen Recording, then restart it"
            );
        }
        if !AXIsProcessTrusted() {
            prompt_accessibility();
            log::warn!(
                "Accessibility permission is missing, so input is ignored: allow voxl (or the \
                 terminal running it) in System Settings > Privacy & Security > Accessibility"
            );
        }
    }
    if let Some(t) = &app {
        // Size the stream for the window: Retina pixels unless that busts the budget.
        match find_window(t) {
            Ok(w) => {
                let d = display_of(&w.frame).unwrap_or(display);
                let c = crop_for(t, &w);
                let mut s = SETTINGS.lock();
                if s.scale.is_none() {
                    s.scale = Some(auto_scale(
                        c.size.width,
                        c.size.height,
                        native_scale(d),
                        s.max_pixels,
                    ));
                }
                drop(s);
                let (pw, ph) = size_for(c.size.width, c.size.height);
                log::info!(
                    "streaming the window \"{}\" of {} ({}, pid {}): {}x{} points, {pw}x{ph} pixels",
                    w.title,
                    w.app,
                    w.bundle,
                    w.pid,
                    c.size.width,
                    c.size.height,
                );
            }
            Err(e) => {
                log::error!("--app={}: {e}", t.query);
                std::process::exit(1);
            }
        }
        return;
    }
    {
        let b = unsafe { CGDisplayBounds(display) };
        let mut s = SETTINGS.lock();
        if s.scale.is_none() {
            s.scale = Some(auto_scale(
                b.size.width,
                b.size.height,
                native_scale(display),
                s.max_pixels,
            ));
        }
    }
    let (w, h) = capture_size(display);
    log::info!(
        "display {display}: {}x{} points, streaming {w}x{h}",
        unsafe { CGDisplayBounds(display) }.size.width,
        unsafe { CGDisplayBounds(display) }.size.height
    );
}

/// `--display` as an index into the active displays (0 = main) or a display ID.
fn pick_display(arg: Option<&str>) -> u32 {
    let ids = active_displays();
    for (i, id) in ids.iter().enumerate() {
        let b = unsafe { CGDisplayBounds(*id) };
        log::info!(
            "display {i}: id {id}, {}x{} points{}",
            b.size.width,
            b.size.height,
            if unsafe { CGDisplayIsMain(*id) } != 0 {
                " (main)"
            } else {
                ""
            }
        );
    }
    let main = unsafe { CGMainDisplayID() };
    match arg.and_then(|s| s.trim_start_matches(':').parse::<u32>().ok()) {
        Some(n) if (n as usize) < ids.len() => ids[n as usize],
        Some(n) if ids.contains(&n) => n,
        Some(n) => {
            log::warn!("no display {n}; using the main display");
            main
        }
        None => main,
    }
}

pub fn active_displays() -> Vec<u32> {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) };
    let mut v = ids[..n as usize].to_vec();
    // Main display first, so index 0 is the main one.
    let main = unsafe { CGMainDisplayID() };
    v.sort_by_key(|&id| id != main);
    v
}

pub fn display() -> u32 {
    SETTINGS.lock().display
}

/// `--app`, when one app's window is streamed instead of a display.
pub fn app_target() -> Option<AppTarget> {
    SETTINGS.lock().app.clone()
}

/// Whether `--app` streams the iOS Simulator (viewers get its device buttons).
pub fn streams_simulator() -> bool {
    app_target()
        .and_then(|t| find_window(&t).ok())
        .is_some_and(|w| w.bundle == sim::BUNDLE_ID)
}

/// The part of window `w` to stream, in global points: with `--sim-screen`, the
/// Simulator's device screen; otherwise the whole window.
pub fn crop_for(t: &AppTarget, w: &stream::WindowInfo) -> CGRect {
    if t.screen_only && w.bundle == sim::BUNDLE_ID {
        if let Some(r) = sim::screen_rect(w.pid, &w.frame) {
            return r;
        }
        log::warn!("couldn't find the device screen in the Simulator window; streaming all of it");
    }
    w.frame
}

fn matches(t: &AppTarget, w: &stream::WindowInfo, exact: bool) -> bool {
    let q = t.query.to_lowercase();
    let (app, bundle) = (w.app.to_lowercase(), w.bundle.to_lowercase());
    let app_ok = if exact {
        app == q || bundle == q
    } else {
        app.contains(&q) || bundle.contains(&q)
    };
    app_ok
        && t.title
            .as_ref()
            .is_none_or(|f| w.title.to_lowercase().contains(f.as_str()))
}

/// The window `--app` (and `--window-title`) picks: an on-screen window of an app
/// whose name or bundle ID matches exactly (else contains the query), the largest if
/// the app has several.
pub fn find_window(t: &AppTarget) -> Result<stream::WindowInfo, String> {
    let all = stream::windows()?;
    let usable = |w: &&stream::WindowInfo| {
        w.on_screen && w.frame.size.width >= 40.0 && w.frame.size.height >= 40.0
    };
    let area = |w: &stream::WindowInfo| w.frame.size.width * w.frame.size.height;
    let pick = |exact: bool| {
        all.iter()
            .filter(usable)
            .filter(|w| matches(t, w, exact))
            .max_by(|a, b| area(a).total_cmp(&area(b)))
            .cloned()
    };
    if let Some(w) = pick(true).or_else(|| pick(false)) {
        return Ok(w);
    }
    let mut names: Vec<String> = all
        .iter()
        .filter(usable)
        .map(|w| format!("{} ({})", w.app, w.bundle))
        .collect();
    names.sort();
    names.dedup();
    Err(format!(
        "no on-screen window matches{}; apps with windows: {}",
        t.title
            .as_ref()
            .map(|f| format!(" (with \"{f}\" in its title)"))
            .unwrap_or_default(),
        names.join(", ")
    ))
}

/// Current bounds (global points) of a window, or None once it is gone.
pub fn window_bounds(id: u32) -> Option<CGRect> {
    unsafe {
        let list = CGWindowListCopyWindowInfo(kCGWindowListOptionIncludingWindow, id);
        if list.is_null() {
            return None;
        }
        let mut out = None;
        for i in 0..CFArrayGetCount(list) {
            let d = CFArrayGetValueAtIndex(list, i);
            let mut n = 0i64;
            let num = CFDictionaryGetValue(d, kCGWindowNumber);
            if num.is_null()
                || !CFNumberGetValue(num, kCFNumberSInt64Type, &mut n as *mut i64 as *mut _)
                || n as u32 != id
            {
                continue;
            }
            let b = CFDictionaryGetValue(d, kCGWindowBounds);
            let mut r = CGRect::default();
            if !b.is_null() && CGRectMakeWithDictionaryRepresentation(b, &mut r) {
                out = Some(r);
            }
        }
        CFRelease(list as CFTypeRef);
        out
    }
}

/// The active display that holds the middle of `r`.
pub fn display_of(r: &CGRect) -> Option<u32> {
    let (cx, cy) = (
        r.origin.x + r.size.width / 2.0,
        r.origin.y + r.size.height / 2.0,
    );
    active_displays().into_iter().find(|&id| {
        let b = unsafe { CGDisplayBounds(id) };
        cx >= b.origin.x
            && cx < b.origin.x + b.size.width
            && cy >= b.origin.y
            && cy < b.origin.y + b.size.height
    })
}

/// With --app: bring the streamed app to the front, so clicks and keys reach it and
/// not a window covering it. Returns whether it had to be raised.
pub fn raise_app(pid: i32) -> bool {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return false;
    };
    if app.isActive() {
        return false;
    }
    #[allow(deprecated)]
    app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
    true
}

pub fn fps() -> u32 {
    SETTINGS.lock().fps
}

/// Pixels per point for a `w`x`h`-point desktop whose panel has `native` pixels per
/// point: native, reduced so a frame stays within --max-capture-pixels (x264's cost
/// grows with area).
fn auto_scale(w: f64, h: f64, native: f64, max_pixels: f64) -> f64 {
    let native = if native > 0.0 { native } else { 1.0 };
    native.min((max_pixels / (w * h).max(1.0)).sqrt()).max(0.25)
}

/// Desktop pixels per point, fixed at startup (--capture-scale or automatic) so every
/// display mode streams at the same scale and the viewer's window maps to points.
fn scale() -> f64 {
    SETTINGS.lock().scale.unwrap_or(1.0)
}

/// Most pixels a frame should have (--max-capture-pixels).
pub fn max_pixels() -> f64 {
    SETTINGS.lock().max_pixels
}

/// Streamed size (even, for 4:2:0) of a desktop of `w`x`h` points.
pub fn size_for(w: f64, h: f64) -> (u32, u32) {
    let k = scale();
    (((w * k).round() as u32) & !1, ((h * k).round() as u32) & !1)
}

fn native_scale(display: u32) -> f64 {
    unsafe {
        let m = CGDisplayCopyDisplayMode(display);
        if m.is_null() {
            return 1.0;
        }
        let k = CGDisplayModeGetPixelWidth(m) as f64 / CGDisplayModeGetWidth(m).max(1) as f64;
        CGDisplayModeRelease(m);
        k
    }
}

/// Desktop pixels per point as streamed.
pub fn capture_scale() -> f64 {
    scale()
}

/// Streamed size of `display`.
pub fn capture_size(display: u32) -> (u32, u32) {
    let b = unsafe { CGDisplayBounds(display) };
    size_for(b.size.width, b.size.height)
}

fn prompt_accessibility() {
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
    unsafe {
        let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
        let opts = CFDictionary::from_slices(&[&*key], &[CFBoolean::new(true)]);
        AXIsProcessTrustedWithOptions(&*opts as *const _ as *const std::ffi::c_void);
    }
}
