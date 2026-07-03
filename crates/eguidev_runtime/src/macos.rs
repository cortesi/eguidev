//! macOS process tweaks for background automation.
//!
//! eframe 0.35 skips `App::ui` and painting for minimized or occluded
//! windows (`ViewportInfo::visible()` gates `run_ui` in the glow and wgpu
//! integrations). Frame-driven automation therefore freezes as soon as the
//! developer's windows fully cover an instrumented app, and no
//! `request_repaint()` can revive it. There is no public eframe or winit
//! switch to override the gate, so while automation is attached we make
//! every window in this process report `NSWindowOcclusionStateVisible`:
//! winit then never emits `Occluded(true)` and eframe keeps running the UI
//! and painting in the background.
//!
//! We also demote the app to the accessory activation policy so launching
//! an instrumented app does not activate it, raise its window, or steal the
//! developer's focus.
//!
//! Set `EGUIDEV_FOREGROUND` in the app environment to skip both tweaks and
//! get ordinary window behavior under automation.

use std::{
    collections::HashMap,
    env,
    ffi::{CStr, c_char},
    mem,
    sync::{Mutex, OnceLock},
};

use objc2::{
    MainThreadMarker, class, msg_send,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};

use crate::viewports::PlatformViewportState;

/// `NSWindowOcclusionStateVisible`.
const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
/// `NSApplicationActivationPolicyAccessory`.
const ACTIVATION_POLICY_ACCESSORY: isize = 1;

type OcclusionStateFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize;

static ORIGINAL_OCCLUSION_STATE: OnceLock<OcclusionStateFn> = OnceLock::new();
static WINDOW_STATES: OnceLock<Mutex<HashMap<usize, PlatformViewportState>>> = OnceLock::new();

/// Install the background-automation tweaks once per process.
pub fn install_background_automation() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let _ = INSTALLED.get_or_init(|| {
        if env::var_os("EGUIDEV_FOREGROUND").is_some() {
            return;
        }
        spoof_occlusion_state();
        demote_activation_policy();
    });
}

pub(crate) fn platform_window_states() -> Vec<PlatformViewportState> {
    let Some(states) = WINDOW_STATES.get() else {
        return Vec::new();
    };
    states
        .lock()
        .expect("platform window states lock poisoned")
        .values()
        .cloned()
        .collect()
}

/// Replace `-[NSWindow occlusionState]` so every window always reports
/// itself visible, keeping eframe rendering when the window is covered.
fn spoof_occlusion_state() {
    unsafe extern "C-unwind" fn always_visible(this: *mut AnyObject, sel: Sel) -> usize {
        let real_state = ORIGINAL_OCCLUSION_STATE
            .get()
            .map(|original| unsafe { original(this, sel) })
            .unwrap_or(OCCLUSION_STATE_VISIBLE);
        record_window_state(this, real_state);
        OCCLUSION_STATE_VISIBLE
    }

    let Some(class) = AnyClass::get(c"NSWindow") else {
        return;
    };
    let Some(method) = class.instance_method(sel!(occlusionState)) else {
        return;
    };
    let imp = always_visible as OcclusionStateFn;
    // SAFETY: the replacement implementation matches the original method
    // signature (no arguments, returns `NSUInteger`) and never unwinds.
    unsafe {
        let original = method.set_implementation(mem::transmute::<OcclusionStateFn, Imp>(imp));
        match ORIGINAL_OCCLUSION_STATE.set(mem::transmute::<Imp, OcclusionStateFn>(original)) {
            Ok(()) | Err(_) => {}
        }
    }
}

fn record_window_state(window: *mut AnyObject, real_state: usize) {
    if window.is_null() {
        return;
    }
    let state = PlatformViewportState {
        title: unsafe { window_title(window) },
        os_minimized: Some(unsafe { window_is_minimized(window) }),
        os_occluded: Some(real_state & OCCLUSION_STATE_VISIBLE == 0),
    };
    let states = WINDOW_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    states
        .lock()
        .expect("platform window states lock poisoned")
        .insert(window as usize, state);
}

unsafe fn window_is_minimized(window: *mut AnyObject) -> bool {
    unsafe { msg_send![window, isMiniaturized] }
}

unsafe fn window_title(window: *mut AnyObject) -> Option<String> {
    let title: *mut AnyObject = unsafe { msg_send![window, title] };
    if title.is_null() {
        return None;
    }
    let bytes: *const c_char = unsafe { msg_send![title, UTF8String] };
    if bytes.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(bytes) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Switch the app to the accessory activation policy and drop any activation
/// acquired during launch, so automation runs never steal focus.
fn demote_activation_policy() {
    let Some(_mtm) = MainThreadMarker::new() else {
        // Attach ran off the main thread; skip rather than race AppKit.
        return;
    };
    // SAFETY: standard AppKit messaging on the main thread; the selectors
    // and argument types match the NSApplication declarations.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: bool = msg_send![app, setActivationPolicy: ACTIVATION_POLICY_ACCESSORY];
        let _: () = msg_send![app, deactivate];
    }
}
