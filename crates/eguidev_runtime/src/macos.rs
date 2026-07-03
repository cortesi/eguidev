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

use std::{env, mem, sync::OnceLock};

use objc2::{
    MainThreadMarker, class, msg_send,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};

/// `NSWindowOcclusionStateVisible`.
const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
/// `NSApplicationActivationPolicyAccessory`.
const ACTIVATION_POLICY_ACCESSORY: isize = 1;

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

/// Replace `-[NSWindow occlusionState]` so every window always reports
/// itself visible, keeping eframe rendering when the window is covered.
fn spoof_occlusion_state() {
    unsafe extern "C-unwind" fn always_visible(_this: *mut AnyObject, _sel: Sel) -> usize {
        OCCLUSION_STATE_VISIBLE
    }

    let Some(class) = AnyClass::get(c"NSWindow") else {
        return;
    };
    let Some(method) = class.instance_method(sel!(occlusionState)) else {
        return;
    };
    let imp = always_visible as unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize;
    // SAFETY: the replacement implementation matches the original method
    // signature (no arguments, returns `NSUInteger`) and never unwinds.
    unsafe {
        let _ = method.set_implementation(mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize,
            Imp,
        >(imp));
    }
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
