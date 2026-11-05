//! The iOS Simulator, when `--app` streams it: the device screen's rectangle inside
//! the Simulator window (so the stream can be cropped to just the phone's screen) and
//! the device's hardware buttons (Home, Lock, volume, Action, rotation, ...), pressed
//! through the Simulator's own menu items with the Accessibility API.
//!
//! Xcode 27 replaced Simulator.app with Device Hub (com.apple.dt.Devices). Its device
//! window has Home, Screenshot, Rotate and, for the foldable iPhone Duo, Closed /
//! Partially Open / Open buttons; volume and orientation are in its Device menu. Its
//! accessibility frames for the device screen aren't in screen coordinates, so the
//! crop is found in the captured image instead (capture.rs).

use super::ffi::*;
use objc2_core_foundation::CFString;
use std::ffi::c_void;

pub const BUNDLE_ID: &str = "com.apple.iphonesimulator";
/// Xcode 27's Device Hub, which runs simulators instead of Simulator.app.
pub const DEVICE_HUB: &str = "com.apple.dt.Devices";

pub fn is_simulator(bundle: &str) -> bool {
    bundle == BUNDLE_ID || bundle == DEVICE_HUB
}

/// When a fold or rotation changed the device's shape inside Device Hub's window, the
/// capture has to find the device again (uptime ms to do it at, 0 for none).
static RECROP_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
fn now_ms() -> u64 {
    EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}
/// Whether a re-crop asked for earlier is due (and clears it).
pub fn take_recrop() -> bool {
    use std::sync::atomic::Ordering;
    let at = RECROP_AT.load(Ordering::Relaxed);
    at != 0 && now_ms() >= at && RECROP_AT.swap(0, Ordering::Relaxed) == at
}
fn recrop_after(ms: u64) {
    RECROP_AT.store(now_ms() + ms, std::sync::atomic::Ordering::Relaxed);
}

type AXUIElementRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        e: AXUIElementRef,
        attr: CFStringRef,
        out: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(e: AXUIElementRef, action: CFStringRef) -> i32;
    fn AXValueGetValue(v: CFTypeRef, ty: u32, out: *mut c_void) -> bool;
    fn AXValueCreate(ty: u32, value: *const c_void) -> CFTypeRef;
    fn AXUIElementSetAttributeValue(e: AXUIElementRef, attr: CFStringRef, value: CFTypeRef) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
}

const K_AX_VALUE_CGPOINT: u32 = 1;
const K_AX_VALUE_CGSIZE: u32 = 2;

/// An owned CF object, released on drop.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn attr(e: AXUIElementRef, name: &str) -> Option<Cf> {
    let key = CFString::from_str(name);
    let mut out: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(e, &*key as *const CFString as CFStringRef, &mut out)
    };
    (err == 0 && !out.is_null()).then_some(Cf(out))
}

fn string_attr(e: AXUIElementRef, name: &str) -> Option<String> {
    let v = attr(e, name)?;
    unsafe { (CFGetTypeID(v.0) == CFStringGetTypeID()).then(|| cf_string(v.0)) }
}

/// Children of an element, valid while the returned array lives.
fn children(e: AXUIElementRef) -> (Option<Cf>, Vec<AXUIElementRef>) {
    let Some(arr) = attr(e, "AXChildren") else {
        return (None, Vec::new());
    };
    let mut v = Vec::new();
    unsafe {
        if CFGetTypeID(arr.0) == CFArrayGetTypeID() {
            for i in 0..CFArrayGetCount(arr.0) {
                v.push(CFArrayGetValueAtIndex(arr.0, i));
            }
        }
    }
    (Some(arr), v)
}

fn frame(e: AXUIElementRef) -> Option<CGRect> {
    let (p, s) = (attr(e, "AXPosition")?, attr(e, "AXSize")?);
    let mut r = CGRect::default();
    unsafe {
        if !AXValueGetValue(
            p.0,
            K_AX_VALUE_CGPOINT,
            &mut r.origin as *mut _ as *mut c_void,
        ) || !AXValueGetValue(s.0, K_AX_VALUE_CGSIZE, &mut r.size as *mut _ as *mut c_void)
        {
            return None;
        }
    }
    Some(r)
}

fn app(pid: i32) -> Option<Cf> {
    let a = unsafe { AXUIElementCreateApplication(pid) };
    (!a.is_null()).then_some(Cf(a))
}

fn near(a: &CGRect, b: &CGRect) -> bool {
    (a.origin.x - b.origin.x).abs() < 3.0
        && (a.origin.y - b.origin.y).abs() < 3.0
        && (a.size.width - b.size.width).abs() < 3.0
        && (a.size.height - b.size.height).abs() < 3.0
}

/// The device screen (global points) inside the Simulator window at `window`: the
/// window's `iOSContentGroup` element.
pub fn screen_rect(pid: i32, window: &CGRect) -> Option<CGRect> {
    let a = app(pid)?;
    let (_wins, wins) = children(a.0);
    for w in wins {
        if string_attr(w, "AXRole").as_deref() != Some("AXWindow") {
            continue;
        }
        if !frame(w).is_some_and(|f| near(&f, window)) {
            continue;
        }
        let (_kids, kids) = children(w);
        for k in kids {
            if string_attr(k, "AXSubrole").as_deref() == Some("iOSContentGroup") {
                return frame(k).filter(|r| r.size.width >= 40.0 && r.size.height >= 40.0);
            }
        }
    }
    None
}

/// Presses the first button titled `title` in any of the app's windows.
fn press_button(pid: i32, title: &str) -> Result<(), String> {
    fn find(e: AXUIElementRef, title: &str, depth: u32) -> Option<(Vec<Cf>, AXUIElementRef)> {
        if string_attr(e, "AXRole").as_deref() == Some("AXButton")
            && (string_attr(e, "AXTitle").as_deref() == Some(title)
                || string_attr(e, "AXDescription").as_deref() == Some(title))
        {
            return Some((Vec::new(), e));
        }
        if depth > 14 {
            return None;
        }
        let (arr, kids) = children(e);
        for k in kids {
            if let Some((mut hold, hit)) = find(k, title, depth + 1) {
                hold.extend(arr);
                return Some((hold, hit));
            }
        }
        None
    }
    let a = app(pid).ok_or("no accessibility access to Device Hub")?;
    let (_hold, hit) = find(a.0, title, 0).ok_or_else(|| format!("no \"{title}\" button"))?;
    let action = CFString::from_static_str("AXPress");
    let err = unsafe { AXUIElementPerformAction(hit, &*action as *const CFString as CFStringRef) };
    if err == 0 {
        Ok(())
    } else {
        Err(format!("pressing \"{title}\" failed ({err})"))
    }
}

fn set_frame(e: AXUIElementRef, r: &CGRect) -> bool {
    unsafe {
        let p = AXValueCreate(K_AX_VALUE_CGPOINT, &r.origin as *const _ as *const c_void);
        let z = AXValueCreate(K_AX_VALUE_CGSIZE, &r.size as *const _ as *const c_void);
        let (pk, sk) = (
            CFString::from_static_str("AXPosition"),
            CFString::from_static_str("AXSize"),
        );
        let ok = !p.is_null()
            && !z.is_null()
            && AXUIElementSetAttributeValue(e, &*pk as *const CFString as CFStringRef, p) == 0
            && AXUIElementSetAttributeValue(e, &*sk as *const CFString as CFStringRef, z) == 0;
        if !p.is_null() {
            CFRelease(p);
        }
        if !z.is_null() {
            CFRelease(z);
        }
        ok
    }
}

/// Lays Device Hub out to stream its device well: the main window (the compact one
/// can't zoom) without the sidebar, zoomed to fit, and large on its display, so the
/// device is drawn with as many pixels as possible. Returns whether anything changed
/// (the window then has to be found again).
pub fn prepare_hub(pid: i32, window: &CGRect) -> bool {
    let mut changed = press_button(pid, "Show in Device Hub").is_ok();
    if changed {
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
    if press_menu(pid, &["View", "Hide Sidebar"]).is_ok() {
        changed = true;
    }
    let _ = press_menu(pid, &["View", "Zoom to Fit"]);
    // Fill most of the display the window is on (it's captured even when covered).
    let d = super::display_of(window).unwrap_or_else(super::display);
    let b = unsafe { CGDisplayBounds(d) };
    let size = CGSize {
        width: (b.size.width - 80.0).min(1900.0),
        height: b.size.height - 70.0,
    };
    let target = CGRect {
        origin: CGPoint {
            x: b.origin.x + (b.size.width - size.width) / 2.0,
            y: b.origin.y + 35.0,
        },
        size,
    };
    if let Some(a) = app(pid) {
        let (_wins, wins) = children(a.0);
        for w in wins {
            if string_attr(w, "AXSubrole").as_deref() != Some("AXStandardWindow") {
                continue;
            }
            let Some(f) = frame(w) else { continue };
            if f.size.width * f.size.height >= size.width * size.height * 0.9 {
                continue;
            }
            if set_frame(w, &target) {
                log::info!(
                    "enlarged Device Hub's window to {:.0}x{:.0} points",
                    size.width,
                    size.height
                );
                changed = true;
            }
        }
    }
    if changed {
        std::thread::sleep(std::time::Duration::from_millis(800));
        let _ = press_menu(pid, &["View", "Zoom to Fit"]);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    changed
}

/// Presses the menu item at `path` (menu bar title, then item titles) in the app.
fn press_menu(pid: i32, path: &[&str]) -> Result<(), String> {
    let a = app(pid).ok_or("no accessibility access to the Simulator")?;
    let bar = attr(a.0, "AXMenuBar").ok_or("the Simulator has no menu bar")?;
    let mut holders: Vec<Option<Cf>> = Vec::new();
    let mut cur = bar.0;
    for (depth, title) in path.iter().enumerate() {
        // Below the menu bar, items sit inside an AXMenu child.
        if depth > 0 {
            let (arr, kids) = children(cur);
            holders.push(arr);
            cur = *kids
                .iter()
                .find(|k| string_attr(**k, "AXRole").as_deref() == Some("AXMenu"))
                .ok_or_else(|| format!("no submenu under {}", path[depth - 1]))?;
        }
        let (arr, kids) = children(cur);
        holders.push(arr);
        cur = *kids
            .iter()
            .find(|k| string_attr(**k, "AXTitle").as_deref() == Some(*title))
            .ok_or_else(|| format!("no menu item \"{title}\""))?;
    }
    let action = CFString::from_static_str("AXPress");
    let err = unsafe { AXUIElementPerformAction(cur, &*action as *const CFString as CFStringRef) };
    drop(holders);
    if err == 0 {
        Ok(())
    } else {
        Err(format!("pressing {} failed ({err})", path.join(" > ")))
    }
}

/// A device action from a viewer (`dv,<action>`), as the Simulator menu item it presses.
fn menu_for(action: &str) -> Option<Vec<&'static str>> {
    Some(match action {
        "home" => vec!["Device", "Home"],
        "lock" => vec!["Device", "Lock"],
        "siri" => vec!["Device", "Siri"],
        "shake" => vec!["Device", "Shake"],
        "action" => vec!["Device", "Action Button"],
        "screenshot" => vec!["Device", "Trigger Screenshot"],
        "rotl" => vec!["Device", "Rotate Left"],
        "rotr" => vec!["Device", "Rotate Right"],
        "volup" => vec!["I/O", "Increase Volume"],
        "voldown" => vec!["I/O", "Decrease Volume"],
        "portrait" => vec!["Device", "Orientation", "Portrait"],
        "upside-down" => vec!["Device", "Orientation", "Portrait Upside Down"],
        "landscape-left" => vec!["Device", "Orientation", "Landscape Left"],
        "landscape-right" => vec!["Device", "Orientation", "Landscape Right"],
        _ => return None,
    })
}

/// What a device action does in Device Hub: a button in the device window, or a menu.
enum HubAction {
    Button(&'static str),
    Menu(Vec<&'static str>),
}

fn hub_action(action: &str) -> Option<HubAction> {
    use HubAction::*;
    Some(match action {
        "home" => Button("Home"),
        "screenshot" => Button("Screenshot"),
        "rotr" => Button("Rotate Right"),
        "fold-closed" => Button("Closed"),
        "fold-half" => Button("Partially Open"),
        "fold-open" => Button("Open"),
        "volup" => Menu(vec!["Device", "Sound", "Increase Volume"]),
        "voldown" => Menu(vec!["Device", "Sound", "Decrease Volume"]),
        "portrait" => Menu(vec!["Device", "Orientation", "Portrait"]),
        "upside-down" => Menu(vec!["Device", "Orientation", "Portrait Upside Down"]),
        "landscape-left" => Menu(vec!["Device", "Orientation", "Landscape Left"]),
        "landscape-right" => Menu(vec!["Device", "Orientation", "Landscape Right"]),
        "rotl" => Menu(vec!["Device", "Orientation", "Landscape Left"]),
        _ => return None,
    })
}

/// The last fold state and orientation pressed in Device Hub.
static LAST_STATE: std::sync::Mutex<[Option<String>; 2]> = std::sync::Mutex::new([None, None]);

/// A non-foldable iPhone mirrors a foldable viewer's hinge with its side button:
/// folding shut locks it, opening it again wakes it.
static LOCKED_BY_FOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Runs a viewer's device action against the streamed Simulator.
pub fn device_action(action: &str) {
    let Some(pid) = super::GEOMETRY.lock().and_then(|g| g.pid) else {
        return;
    };
    let bundle = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|a| a.bundleIdentifier())
        .map(|b| b.to_string())
        .unwrap_or_default();
    if bundle == DEVICE_HUB {
        // A foldable (iPhone Duo) folds like the viewer's phone; anything else locks.
        let action = match action {
            "posture-closed" => "fold-closed",
            "posture-half" => "fold-half",
            "posture-open" => "fold-open",
            a => a,
        };
        let Some(a) = hub_action(action) else {
            log::info!("device action {action:?} isn't available in Device Hub");
            return;
        };
        // Orientation and fold are states: pressing one the device is already in
        // would only restart the stream (and two viewers would ping-pong it).
        if matches!(
            action,
            "portrait" | "upside-down" | "landscape-left" | "landscape-right"
        ) || action.starts_with("fold-")
        {
            let mut last = LAST_STATE.lock().unwrap();
            let key = if action.starts_with("fold-") { 0 } else { 1 };
            if last[key].as_deref() == Some(action) {
                return;
            }
            last[key] = Some(action.to_string());
        }
        let reshapes = action.starts_with("fold-")
            || action.starts_with("rot")
            || matches!(
                action,
                "portrait" | "upside-down" | "landscape-left" | "landscape-right"
            );
        let action = action.to_string();
        std::thread::spawn(move || {
            let r = match &a {
                HubAction::Button(t) => {
                    log::info!("device action: {t} button");
                    press_button(pid, t)
                }
                HubAction::Menu(p) => {
                    log::info!("device action: {}", p.join(" > "));
                    press_menu(pid, p)
                }
            };
            if action.starts_with("rot") {
                LAST_STATE.lock().unwrap()[1] = None;
            }
            match r {
                // The device changes shape; find it again once the animation settles.
                Ok(()) if reshapes => recrop_after(1300),
                Ok(()) => {}
                Err(e) if action.starts_with("fold-") => {
                    log::info!("{e}: not a foldable device")
                }
                Err(e) => log::warn!("device action {action}: {e}"),
            }
        });
        return;
    }
    // The old Simulator: a fold of the viewer's phone locks / wakes the iPhone.
    let action = match action {
        "posture-closed" => {
            if LOCKED_BY_FOLD.swap(true, std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            "lock"
        }
        "posture-open" => {
            if !LOCKED_BY_FOLD.swap(false, std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            "lock"
        }
        "posture-half" => return,
        a => a,
    };
    let Some(path) = menu_for(action) else {
        log::debug!("unknown device action {action:?}");
        return;
    };
    log::info!("device action: {}", path.join(" > "));
    // Accessibility calls can block for a while; keep them off the socket task.
    std::thread::spawn(move || {
        if let Err(e) = press_menu(pid, &path) {
            log::warn!("device action {}: {e}", path.join(" > "));
        }
    });
}
