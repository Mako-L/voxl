//! Screen capture with ScreenCaptureKit. Frames arrive as NV12 (converted on the GPU,
//! BT.601 like the Linux path) together with the rectangles that changed, which drive
//! the same damage bookkeeping as XDamage: nothing is converted or encoded while the
//! screen is still, and x264 skips the macroblocks that didn't change.

use super::ffi::*;
use super::stream::{self, OutputType, Source};
use super::Geometry;
use nix::unistd::{read, write};
use objc2_core_foundation::CFString;
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCStreamFrameInfoDirtyRects, SCStreamFrameInfoStatus,
};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub static DAMAGE_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Set once the first capture started: from then on a missing --app window is waited
/// for (the app restarting, the Simulator switching devices) instead of failing.
static STARTED: AtomicBool = AtomicBool::new(false);

/// (x0, y0, x1, y1) in frame pixels.
type PixelRect = (usize, usize, usize, usize);

/// The screen inside a device drawn by Device Hub (`x0..x1, y0..y1` its outline in
/// frame pixels): past the frame and the pure black ring of the bezel, and trimmed by
/// the rounded corners, so what's left is all screen, edge to edge. Each edge is
/// measured along several lines and the median taken, so dark content touching the
/// bezel on a few of them doesn't matter.
/// How often to look at Device Hub's window again for the device.
const RECHECK_EVERY: Duration = Duration::from_secs(2);

fn screen_inside(
    at: &dyn Fn(usize, usize) -> i32,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> PixelRect {
    // Video-range luma: black is 16.
    const BLACK: i32 = 20;
    let (w, h) = (x1 - x0, y1 - y0);
    // Walking from `start` by `step` (pixels as a function of the step count): the
    // number of steps until the screen starts, or None when the black run looks like
    // dark content rather than bezel.
    let inset = |px: &dyn Fn(usize) -> i32, max: usize| -> Option<usize> {
        let mut i = 0;
        while i < max && px(i) > BLACK {
            i += 1; // the frame
        }
        let ring = i;
        while i < max && px(i) <= BLACK {
            i += 1; // the black ring
        }
        (i > ring && i < max).then_some(i)
    };
    let median = |mut v: Vec<usize>| -> Option<usize> {
        v.sort_unstable();
        (!v.is_empty()).then(|| v[v.len() / 2])
    };
    let lines = |n: usize| (2..=8).map(move |k| n * k / 10);
    let (mw, mh) = (w / 8, h / 8);
    let l = median(
        lines(h)
            .filter_map(|r| inset(&|i| at(x0 + i, y0 + r), mw))
            .collect(),
    );
    let rr = median(
        lines(h)
            .filter_map(|r| inset(&|i| at(x1 - 1 - i, y0 + r), mw))
            .collect(),
    );
    let t = median(
        lines(w)
            .filter_map(|c| inset(&|i| at(x0 + c, y0 + i), mh))
            .collect(),
    );
    let b = median(
        lines(w)
            .filter_map(|c| inset(&|i| at(x0 + c, y1 - 1 - i), mh))
            .collect(),
    );
    let any = [l, rr, t, b].into_iter().flatten().max().unwrap_or(0);
    let (l, rr, t, b) = (
        l.unwrap_or(any),
        rr.unwrap_or(any),
        t.unwrap_or(any),
        b.unwrap_or(any),
    );
    // Rounded corners: along each diagonal the screen starts further in than along the
    // edges; trimming that much off every side leaves no corner showing.
    let m = w.min(h) / 4;
    let corners: Vec<usize> = [
        (x0 + l, y0 + t, 1i64, 1i64),
        (x1 - 1 - rr, y0 + t, -1, 1),
        (x0 + l, y1 - 1 - b, 1, -1),
        (x1 - 1 - rr, y1 - 1 - b, -1, -1),
    ]
    .into_iter()
    .filter_map(|(cx, cy, dx, dy)| {
        // Past the frame and the black of the rounded corner, as along the edges.
        inset(
            &|i| {
                at(
                    (cx as i64 + dx * i as i64) as usize,
                    (cy as i64 + dy * i as i64) as usize,
                )
            },
            m,
        )
    })
    .collect();
    let c = median(corners).unwrap_or(0) + 1;
    let (a0, b0) = (x0 + l + c, y0 + t + c);
    let (a1, b1) = (x1 - rr - c, y1 - b - c);
    if a1 > a0 + 40 && b1 > b0 + 40 {
        (a0 & !1, b0 & !1, a1 & !1, b1 & !1)
    } else {
        (x0, y0, x1, y1)
    }
}

/// Device Hub (Xcode 27) draws the device centred on a plain background inside its
/// window, and its accessibility frames for the screen aren't in screen coordinates.
/// So: capture the whole window once and take the bounding box of whatever differs
/// from the background, between the title bar and the bottom toolbar. Returns it in
/// global points.
/// What a look at Device Hub's window found: the device's screen (global points) and
/// how many times bigger the device could be drawn and still fit (0 when it doesn't).
#[derive(Clone, Copy)]
struct Found {
    screen: CGRect,
    room: f64,
}

/// Finds the device in Device Hub's window, first zooming the canvas so the device is
/// drawn as large as fits (more pixels to stream): in while there's room for another
/// step (about 1.25x), out when it's cut off. Returns its screen in global points.
fn fit_hub(w: &stream::WindowInfo) -> Option<CGRect> {
    const STEP: f64 = 1.3;
    let mut last = None;
    for _ in 0..10 {
        let f = detect_device_rect(w);
        let zoom_in = match f {
            Some(f) if f.room >= STEP => true,
            Some(f) if f.room >= 1.0 => return Some(f.screen),
            // Too big (cut off, or grown into a side panel): back a step.
            _ => false,
        };
        if !zoom_in && last == Some(true) {
            // Just overshot: one step out fits, by the room measured before.
            super::sim::hub_zoom(w.pid, false);
            std::thread::sleep(Duration::from_millis(700));
            return detect_device_rect(w).map(|f| f.screen);
        }
        if !super::sim::hub_zoom(w.pid, zoom_in) {
            return f.map(|f| f.screen);
        }
        last = Some(zoom_in);
        std::thread::sleep(Duration::from_millis(700));
    }
    detect_device_rect(w).map(|f| f.screen)
}

fn detect_device_rect(w: &stream::WindowInfo) -> Option<Found> {
    let (pw, ph) = super::size_for(w.frame.size.width, w.frame.size.height);
    let k = pw as f64 / w.frame.size.width.max(1.0);
    let found: Arc<Mutex<Option<(PixelRect, f64)>>> = Arc::new(Mutex::new(None));
    let f2 = found.clone();
    let on_frame = move |sb: CMSampleBufferRef, ty: OutputType| unsafe {
        if ty != OutputType::Screen || !CMSampleBufferIsValid(sb) || f2.lock().is_some() {
            return;
        }
        let att = CMSampleBufferGetSampleAttachmentsArray(sb, false);
        if att.is_null() || CFArrayGetCount(att) == 0 {
            return;
        }
        let info = CFArrayGetValueAtIndex(att, 0);
        if dict_i64(info, SCStreamFrameInfoStatus as *const _ as *const c_void) != Some(0) {
            return;
        }
        let pb = CMSampleBufferGetImageBuffer(sb);
        if pb.is_null() {
            return;
        }
        CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
        let (w, h) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
        let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
        let y = std::slice::from_raw_parts(CVPixelBufferGetBaseAddressOfPlane(pb, 0), stride * h);
        let at = |x: usize, r: usize| y[r * stride + x] as i32;
        // Toolbar above, controls (and tips that pop up over them) below; the
        // background colour from the left margin.
        let (top, bot) = ((60.0 * k) as usize, h.saturating_sub((110.0 * k) as usize));
        let bg = at((8.0 * k) as usize, h / 2);
        let differs = |x: usize, r: usize| (at(x, r) - bg).abs() > 14;
        // Columns with something on them, grouped into bands. A band that reaches the
        // window's left or right edge is a side panel (the sidebar, or the inspector
        // with Appearance, Location...), never the device: drop those, keep the widest.
        let gap = ((12.0 * k) as usize).max(4);
        let edge = ((6.0 * k) as usize).max(2);
        let mut bands: Vec<(usize, usize)> = Vec::new();
        for x in (0..w).step_by(2) {
            let n = (top..bot).step_by(2).filter(|&r| differs(x, r)).count();
            if n * 2 > (bot - top) / 50 {
                match bands.last_mut() {
                    Some(b) if x <= b.1 + gap => b.1 = x,
                    _ => bands.push((x, x)),
                }
            }
        }
        // The canvas between the panels: how big the device could get.
        let cx0 = bands.iter().find(|b| b.0 <= edge).map_or(0, |b| b.1);
        let cx1 = bands.iter().find(|b| b.1 + edge >= w).map_or(w, |b| b.0);
        let (mut x0, mut x1) = bands
            .into_iter()
            .filter(|&(a, b)| a > edge && b + edge < w)
            .max_by_key(|&(a, b)| b - a)
            .unwrap_or((usize::MAX, 0));
        let (mut y0, mut y1) = (usize::MAX, 0);
        if x0 < x1 {
            for r in (top..bot).step_by(2) {
                let n = (x0..=x1).step_by(2).filter(|&x| differs(x, r)).count();
                if n * 2 > (x1 - x0) / 50 {
                    y0 = y0.min(r);
                    y1 = y1.max(r);
                }
            }
            // The device's own outline within those rows.
            let (mut a, mut b) = (usize::MAX, 0);
            if y0 < y1 {
                for x in (x0..=x1).step_by(2) {
                    let n = (y0..=y1).step_by(2).filter(|&r| differs(x, r)).count();
                    if n * 2 > (y1 - y0) / 50 {
                        a = a.min(x);
                        b = b.max(x);
                    }
                }
            }
            (x0, x1) = (a, b);
        }
        let found = (x0 < x1 && y0 < y1).then(|| {
            // Cut off by the toolbars (zoomed in too far): no room at all.
            let clipped = y0 <= top + 4 || y1 + 6 >= bot;
            let room = if clipped {
                0.0
            } else {
                ((cx1.saturating_sub(cx0)) as f64 / (x1 - x0) as f64)
                    .min((bot - top) as f64 / (y1 - y0) as f64)
            };
            (
                screen_inside(&at, x0, y0, (x1 + 2).min(w), (y1 + 2).min(h)),
                room,
            )
        });
        CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
        if let Some(r) = found {
            *f2.lock() = Some(r);
        }
    };
    let fps = 30;
    let _stream = stream::start(
        Source::Window(w.id),
        "locate",
        &[OutputType::Screen],
        |cfg| unsafe {
            cfg.setWidth(pw as usize);
            cfg.setHeight(ph as usize);
            cfg.setMinimumFrameInterval(objc2_core_media::CMTime {
                value: 1,
                timescale: fps,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            });
            cfg.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            cfg.setShowsCursor(false);
            if objc2_foundation::NSObjectProtocol::respondsToSelector(
                cfg,
                objc2::sel!(setIgnoreShadowsSingleWindow:),
            ) {
                cfg.setIgnoreShadowsSingleWindow(true);
            }
        },
        on_frame,
    )
    .ok()?;
    let t = Instant::now();
    while found.lock().is_none() && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let ((x0, y0, x1, y1), room) = (*found.lock())?;
    let r = CGRect {
        origin: CGPoint {
            x: w.frame.origin.x + x0 as f64 / k,
            y: w.frame.origin.y + y0 as f64 / k,
        },
        size: CGSize {
            width: (x1 - x0) as f64 / k,
            height: (y1 - y0) as f64 / k,
        },
    };
    log::info!(
        "device screen found in Device Hub's window at {:.0},{:.0} {:.0}x{:.0} points",
        r.origin.x - w.frame.origin.x,
        r.origin.y - w.frame.origin.y,
        r.size.width,
        r.size.height
    );
    Some(Found { screen: r, room })
}

/// The window --app streams. At startup a missing window is an error; afterwards
/// capture waits for one to show up again.
fn wait_for_window(t: &super::AppTarget) -> Result<stream::WindowInfo, String> {
    let t0 = Instant::now();
    let mut said = false;
    loop {
        match super::find_window(t) {
            Ok(w) => {
                if said {
                    log::info!("window of {} is back", w.app);
                }
                return Ok(w);
            }
            Err(e) if !STARTED.load(Ordering::Relaxed) => return Err(e),
            Err(e) => {
                if !said || t0.elapsed() > Duration::from_secs(30) {
                    log::warn!("--app={}: {e}; waiting for a window", t.query);
                    said = true;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub y0: u32,
    pub y1: u32,
}

/// A retained ScreenCaptureKit frame, read-locked while it is the current one.
struct Frame {
    pb: CVPixelBufferRef,
    locked: bool,
}
unsafe impl Send for Frame {}

impl Frame {
    fn retain(pb: CVPixelBufferRef) -> Self {
        unsafe { CVPixelBufferRetain(pb) };
        Frame { pb, locked: false }
    }
    fn lock(&mut self) {
        if !self.locked {
            unsafe { CVPixelBufferLockBaseAddress(self.pb, kCVPixelBufferLock_ReadOnly) };
            self.locked = true;
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            if self.locked {
                CVPixelBufferUnlockBaseAddress(self.pb, kCVPixelBufferLock_ReadOnly);
            }
            CVPixelBufferRelease(self.pb);
        }
    }
}

/// Handed from the capture queue to the capture thread.
#[derive(Default)]
struct Incoming {
    frame: Option<Frame>,
    /// Changed areas as (x0, y0, x1, y1) in frame pixels.
    rects: Vec<(u32, u32, u32, u32)>,
}

pub struct Capturer {
    pub width: u32,
    pub height: u32,
    geometry: Geometry,
    incoming: Arc<Mutex<Incoming>>,
    wake_r: OwnedFd,
    /// The frame conversions read from.
    current: Option<Frame>,
    /// Newest frame, adopted at the next grab.
    next: Option<Frame>,
    pub damaged: bool,
    /// When the first damage since the last take arrived.
    pub damaged_at: Option<Instant>,
    acc: Option<(u32, u32)>,
    mb_dirty: Vec<u8>,
    pub mb_cols: usize,
    /// Areas (x0, y0, x1, y1) changed since the video thread last took them, for `/wait`.
    pub changes: Vec<(u32, u32, u32, u32)>,
    stream: stream::Stream,
    last_check: Instant,
    rebuild: bool,
    /// With --app: the CGWindowID being streamed.
    window: Option<u32>,
    /// With --app: the window's frame (global points); `geometry.bounds` is the part
    /// of it that is streamed.
    win_frame: CGRect,
    /// Device Hub: the window (for finding the device again) and the answer.
    win_info: Option<stream::WindowInfo>,
    recrop: Arc<Mutex<Option<Option<CGRect>>>>,
    recrop_busy: Arc<std::sync::atomic::AtomicBool>,
    last_recrop: Instant,
}

/// Number from a CFNumber value of a frame-info dictionary.
unsafe fn dict_i64(dict: CFDictionaryRef, key: *const c_void) -> Option<i64> {
    let v = CFDictionaryGetValue(dict, key);
    let mut out = 0i64;
    (!v.is_null() && CFNumberGetValue(v, kCFNumberSInt64Type, &mut out as *mut i64 as *mut c_void))
        .then_some(out)
}

impl Capturer {
    pub fn new(_display: Option<&str>) -> Result<Self, String> {
        let display = super::display();
        let mut hub_info = None;
        let (source, geometry, window) = match super::app_target() {
            Some(t) => {
                let mut w = wait_for_window(&t)?;
                let hub = w.bundle == super::sim::DEVICE_HUB;
                if hub && t.screen_only && super::sim::prepare_hub(w.pid, &w.frame) {
                    w = wait_for_window(&t)?;
                }
                let crop = if hub && t.screen_only {
                    fit_hub(&w).unwrap_or(w.frame)
                } else {
                    super::crop_for(&t, &w)
                };
                let (width, _) = super::size_for(crop.size.width, crop.size.height);
                let g = Geometry {
                    display: super::display_of(&w.frame).unwrap_or(display),
                    bounds: crop,
                    width,
                    pid: Some(w.pid),
                };
                // Have the app in front from the start, so the first tap lands in it.
                super::raise_app(w.pid);
                hub_info = (hub && t.screen_only).then(|| w.clone());
                (Source::Window(w.id), g, Some((w.id, w.frame)))
            }
            None => {
                let (width, _) = super::capture_size(display);
                let g = Geometry {
                    display,
                    bounds: unsafe { CGDisplayBounds(display) },
                    width,
                    pid: None,
                };
                (Source::Display(display), g, None)
            }
        };
        let (width, height) =
            super::size_for(geometry.bounds.size.width, geometry.bounds.size.height);
        let win_frame = window.map(|(_, f)| f).unwrap_or(geometry.bounds);
        // Part of the window to stream, in the window's own points.
        let crop_src = (window.is_some() && geometry.bounds != win_frame).then(|| {
            objc2_core_foundation::CGRect::new(
                objc2_core_foundation::CGPoint::new(
                    geometry.bounds.origin.x - win_frame.origin.x,
                    geometry.bounds.origin.y - win_frame.origin.y,
                ),
                objc2_core_foundation::CGSize::new(
                    geometry.bounds.size.width,
                    geometry.bounds.size.height,
                ),
            )
        });
        let window = window.map(|(id, _)| id);
        *super::GEOMETRY.lock() = Some(geometry);
        let fps = super::fps();
        let incoming = Arc::new(Mutex::new(Incoming::default()));
        let (wake_r, wake_w) = crate::hub::wake_pipe().map_err(|e| e.to_string())?;
        let debug = std::env::var("VOXL_DEBUG_DAMAGE").is_ok();
        let inc = incoming.clone();
        let (w, h) = (width, height);
        let on_frame = move |sb: CMSampleBufferRef, ty: OutputType| unsafe {
            if ty != OutputType::Screen || !CMSampleBufferIsValid(sb) {
                return;
            }
            let att = CMSampleBufferGetSampleAttachmentsArray(sb, false);
            if att.is_null() || CFArrayGetCount(att) == 0 {
                return;
            }
            let info = CFArrayGetValueAtIndex(att, 0);
            let status_key = SCStreamFrameInfoStatus as *const _ as *const c_void;
            // Only complete frames carry new pixels (idle ones repeat the last).
            if dict_i64(info, status_key) != Some(0) {
                return;
            }
            let pb = CMSampleBufferGetImageBuffer(sb);
            if pb.is_null() {
                return;
            }
            let mut rects = Vec::new();
            let list = CFDictionaryGetValue(info, SCStreamFrameInfoDirtyRects as *const _ as _);
            if list.is_null() {
                rects.push((0, 0, w, h));
            } else {
                for i in 0..CFArrayGetCount(list) {
                    let mut r = CGRect::default();
                    if CGRectMakeWithDictionaryRepresentation(
                        CFArrayGetValueAtIndex(list, i),
                        &mut r,
                    ) {
                        let x0 = r.origin.x.floor().clamp(0.0, w as f64) as u32;
                        let y0 = r.origin.y.floor().clamp(0.0, h as f64) as u32;
                        let x1 = (r.origin.x + r.size.width).ceil().clamp(0.0, w as f64) as u32;
                        let y1 = (r.origin.y + r.size.height).ceil().clamp(0.0, h as f64) as u32;
                        if x1 > x0 && y1 > y0 {
                            rects.push((x0, y0, x1, y1));
                        }
                    }
                }
            }
            if debug {
                log::info!(
                    "frame {}x{} dirty {:?}",
                    CVPixelBufferGetWidth(pb),
                    CVPixelBufferGetHeight(pb),
                    rects
                );
            }
            {
                let mut inc = inc.lock();
                inc.frame = Some(Frame::retain(pb));
                inc.rects.extend(rects);
            }
            let _ = write(&wake_w, &[1u8]);
        };
        let stream = stream::start(
            source,
            "capture",
            &[OutputType::Screen],
            |cfg| unsafe {
                cfg.setWidth(width as usize);
                cfg.setHeight(height as usize);
                cfg.setMinimumFrameInterval(objc2_core_media::CMTime {
                    value: 1,
                    timescale: fps as i32,
                    flags: objc2_core_media::CMTimeFlags::Valid,
                    epoch: 0,
                });
                cfg.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
                cfg.setColorMatrix(&*(kCGDisplayStreamYCbCrMatrix_ITU_R_601_4 as *const CFString));
                // Convert to sRGB on the GPU: without this, wide-gamut displays (P3) are
                // captured in their native space and shown desaturated as BT.709/sRGB.
                cfg.setColorSpaceName(&*(kCGColorSpaceSRGB as *const CFString));
                // Clients draw the cursor themselves (see desktop.rs), so pointer motion
                // costs no encode and has no stream latency.
                // If the system cursor can't be read, it has to be in the video.
                cfg.setShowsCursor(!super::desktop::cursor_readable());
                cfg.setCaptureResolution(SCCaptureResolutionType::Best);
                cfg.setQueueDepth(6);
                if let Some(r) = crop_src {
                    cfg.setSourceRect(r);
                }
                // A single window: just its pixels, without the drop shadow around it
                // (macOS 14+).
                if window.is_some()
                    && objc2_foundation::NSObjectProtocol::respondsToSelector(
                        cfg,
                        objc2::sel!(setIgnoreShadowsSingleWindow:),
                    )
                {
                    cfg.setIgnoreShadowsSingleWindow(true);
                }
            },
            on_frame,
        )?;
        let mut me = Capturer {
            width,
            height,
            geometry,
            incoming,
            wake_r,
            current: None,
            next: None,
            damaged: true,
            damaged_at: None,
            acc: None,
            mb_dirty: vec![0; (width as usize).div_ceil(16) * (height as usize).div_ceil(16)],
            mb_cols: (width as usize).div_ceil(16),
            changes: Vec::new(),
            stream,
            last_check: Instant::now(),
            rebuild: false,
            window,
            win_frame,
            win_info: hub_info,
            recrop: Arc::new(Mutex::new(None)),
            recrop_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_recrop: Instant::now(),
        };
        // The stream opens with a full frame; have it before anything is converted.
        let t = Instant::now();
        while me.next.is_none() && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
            me.pump()?;
        }
        if me.next.is_none() {
            log::warn!("no frame from ScreenCaptureKit yet");
        }
        me.grab(me.full())?;
        STARTED.store(true, Ordering::Relaxed);
        match window {
            Some(id) => {
                log::info!("capturing window {id} with ScreenCaptureKit at {width}x{height}")
            }
            None => {
                log::info!("capturing display {display} with ScreenCaptureKit at {width}x{height}")
            }
        }
        Ok(me)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.wake_r.as_fd()
    }

    /// Take frames and changed areas that arrived since the last call.
    pub fn pump(&mut self) -> Result<(), String> {
        let mut drain = [0u8; 64];
        while matches!(read(&self.wake_r, &mut drain), Ok(n) if n > 0) {}
        let rects = {
            let mut inc = self.incoming.lock();
            if let Some(f) = inc.frame.take() {
                self.next = Some(f);
            }
            std::mem::take(&mut inc.rects)
        };
        for (x0, y0, x1, y1) in rects {
            DAMAGE_EVENTS.fetch_add(1, Ordering::Relaxed);
            let (c0, c1) = (x0 as usize / 16, (x1 as usize).div_ceil(16));
            for row in (y0 as usize / 16)..(y1 as usize).div_ceil(16) {
                self.mb_dirty[row * self.mb_cols + c0..row * self.mb_cols + c1].fill(1);
            }
            self.acc = Some(match self.acc {
                Some((a, b)) => (a.min(y0), b.max(y1)),
                None => (y0, y1),
            });
            self.changes.push((x0, y0, x1, y1));
            self.damaged = true;
            self.damaged_at.get_or_insert_with(Instant::now);
        }
        // Every couple of seconds, and soon after a fold or rotation, look for the
        // device again (off this thread: zooming, folding, rotating, a panel opening all
        // move it without the window changing) and restart only if its outline moved.
        let due = super::sim::take_recrop() || self.last_recrop.elapsed() >= RECHECK_EVERY;
        if due && !self.recrop_busy.load(Ordering::Relaxed) {
            if let Some(w) = self.win_info.clone() {
                self.last_recrop = Instant::now();
                self.recrop_busy.store(true, Ordering::Relaxed);
                let slot = self.recrop.clone();
                let busy = self.recrop_busy.clone();
                std::thread::spawn(move || {
                    *slot.lock() = Some(fit_hub(&w));
                    busy.store(false, Ordering::Relaxed);
                });
            }
        }
        if let Some(found) = self.recrop.lock().take() {
            let b = self.geometry.bounds;
            // Finding the outline wobbles by a few points between looks: ignore that.
            let tol = (b.size.width.max(b.size.height) * 0.015).max(6.0);
            let moved = found.is_some_and(|r| {
                (r.origin.x - b.origin.x).abs() > tol
                    || (r.origin.y - b.origin.y).abs() > tol
                    || (r.size.width - b.size.width).abs() > tol
                    || (r.size.height - b.size.height).abs() > tol
            });
            if moved {
                log::info!("device changed shape; restarting capture");
                self.rebuild = true;
            }
        }
        if let Some(id) = self.window {
            // The window moves (input has to follow it), resizes or closes.
            if self.last_check.elapsed() >= Duration::from_millis(250) {
                self.last_check = Instant::now();
                match super::window_bounds(id) {
                    None => {
                        log::info!("streamed window closed; looking for another");
                        self.rebuild = true;
                    }
                    // Resized (the Simulator rotating, a new device): crop and size anew.
                    Some(b) if b.size != self.win_frame.size => {
                        log::info!(
                            "streamed window resized to {}x{} points; restarting capture",
                            b.size.width,
                            b.size.height
                        );
                        self.rebuild = true;
                    }
                    Some(b) if b.origin != self.win_frame.origin => {
                        // Moved: the streamed part moves with it.
                        self.geometry.bounds.origin.x += b.origin.x - self.win_frame.origin.x;
                        self.geometry.bounds.origin.y += b.origin.y - self.win_frame.origin.y;
                        self.win_frame = b;
                        if let Some(d) = super::display_of(&b) {
                            self.geometry.display = d;
                        }
                        *super::GEOMETRY.lock() = Some(self.geometry);
                    }
                    Some(_) => {}
                }
            }
        } else if self.last_check.elapsed() >= Duration::from_secs(1) {
            self.last_check = Instant::now();
            // Resolution changed (System Settings, another display arrangement).
            let b = unsafe { CGDisplayBounds(self.geometry.display) };
            if b.size != self.geometry.bounds.size
                || super::capture_size(self.geometry.display) != (self.width, self.height)
            {
                log::info!("display changed; restarting capture");
                self.rebuild = true;
            } else if b.origin != self.geometry.bounds.origin {
                self.geometry.bounds = b;
                *super::GEOMETRY.lock() = Some(self.geometry);
            }
        }
        Ok(())
    }

    /// Take accumulated damage as a row band.
    pub fn take_damage(&mut self) -> Result<Option<Band>, String> {
        self.damaged = false;
        self.damaged_at = None;
        let Some((y0, y1)) = self.acc.take() else {
            return Ok(None);
        };
        Ok(Some(Band {
            y0: y0 & !1,
            y1: ((y1 + 1) & !1).min(self.height),
        }))
    }

    /// Take the dirty-macroblock map accumulated with the current damage.
    pub fn take_mb_map(&mut self) -> Vec<u8> {
        let n = self.mb_dirty.len();
        std::mem::replace(&mut self.mb_dirty, vec![0; n])
    }

    pub fn full(&self) -> Band {
        Band {
            y0: 0,
            y1: self.height,
        }
    }

    /// Make the newest frame the current one. Every ScreenCaptureKit frame is a whole
    /// screen, so the band doesn't matter.
    pub fn grab(&mut self, _band: Band) -> Result<(), String> {
        if let Some(mut f) = self.next.take() {
            f.lock();
            self.current = Some(f);
        }
        Ok(())
    }

    /// Convert rows [y0, y1) of the current frame into `dst`.
    pub fn convert(&self, dst: &mut crate::convert::I420, y0: usize, y1: usize, threads: usize) {
        let Some(f) = &self.current else {
            return;
        };
        unsafe {
            let pb = f.pb;
            if CVPixelBufferGetWidth(pb) != self.width as usize
                || CVPixelBufferGetHeight(pb) != self.height as usize
            {
                return;
            }
            crate::convert::convert_nv12_band(
                CVPixelBufferGetBaseAddressOfPlane(pb, 0),
                CVPixelBufferGetBytesPerRowOfPlane(pb, 0),
                CVPixelBufferGetBaseAddressOfPlane(pb, 1),
                CVPixelBufferGetBytesPerRowOfPlane(pb, 1),
                dst,
                y0,
                y1.min(self.height as usize),
                threads,
            );
        }
    }

    /// The current frame as packed BGRX (`width * 4` bytes per row), for screenshots.
    pub fn snapshot(&mut self) -> Result<Vec<u8>, String> {
        // Each frame arrives together with its changed areas, so taking them and then
        // adopting the newest frame leaves `changes` holding the damage for exactly
        // these pixels.
        self.pump()?;
        self.grab(self.full())?;
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![0u8; w * h * 4];
        let Some(f) = &self.current else {
            return Ok(out);
        };
        unsafe {
            let pb = f.pb;
            if CVPixelBufferGetWidth(pb) != w || CVPixelBufferGetHeight(pb) != h {
                return Ok(out);
            }
            let (ys, uvs) = (
                CVPixelBufferGetBytesPerRowOfPlane(pb, 0),
                CVPixelBufferGetBytesPerRowOfPlane(pb, 1),
            );
            let y = std::slice::from_raw_parts(CVPixelBufferGetBaseAddressOfPlane(pb, 0), ys * h);
            let uv = std::slice::from_raw_parts(
                CVPixelBufferGetBaseAddressOfPlane(pb, 1),
                uvs * h.div_ceil(2),
            );
            crate::convert::nv12_to_bgrx(y, ys, uv, uvs, w, h, &mut out);
        }
        Ok(out)
    }

    /// Frames are NV12 here, so the BGRX debug marker probe doesn't apply.
    pub fn bgrx(&self) -> Option<(&[u8], usize)> {
        None
    }

    /// The display changed or the stream ended: capture must be set up again.
    pub fn needs_rebuild(&self) -> bool {
        self.rebuild || self.stream.stopped.load(Ordering::Relaxed)
    }
}
