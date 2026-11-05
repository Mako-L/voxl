//! The iOS Simulator, when `--app` streams it: the device screen's rectangle inside
//! the Simulator window (so the stream can be cropped to just the phone's screen) and
//! the device's hardware buttons (Home, Lock, volume, Action, rotation, ...), pressed
//! through the Simulator's own menu items with the Accessibility API.

use super::ffi::*;
use objc2_core_foundation::CFString;
use std::ffi::c_void;

pub const BUNDLE_ID: &str = "com.apple.iphonesimulator";

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

/// Runs a viewer's device action against the streamed Simulator.
pub fn device_action(action: &str) {
    let Some(pid) = super::GEOMETRY.lock().and_then(|g| g.pid) else {
        return;
    };
    let Some(path) = menu_for(action) else {
        log::debug!("unknown device action {action:?}");
        return;
    };
    // Accessibility calls can block for a while; keep them off the socket task.
    std::thread::spawn(move || {
        if let Err(e) = press_menu(pid, &path) {
            log::warn!("device action {}: {e}", path.join(" > "));
        }
    });
}
