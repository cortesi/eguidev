//! macOS presentation and occlusion support for connected automation.
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
//! The process-wide window hook is installed when instrumentation attaches, but
//! its visible-state override is enabled only for a connected MCP session.

#[cfg(test)]
use std::path::{Path, PathBuf};
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, c_char},
    mem, process, ptr,
    ptr::NonNull,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

use block2::RcBlock;
use core_foundation::{
    base::{CFRange, CFType, TCFType},
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_graphics::{
    base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst},
    display::CGRectNull,
    image::CGImage,
    sys::CGImageRef,
    window::{
        self, CGWindowID, create_image, kCGNullWindowID, kCGWindowImageBestResolution,
        kCGWindowImageBoundsIgnoreFraming, kCGWindowListOptionAll,
        kCGWindowListOptionIncludingWindow, kCGWindowName, kCGWindowNumber, kCGWindowOwnerPID,
    },
};
use dispatch2::DispatchQueue;
use eguidev::internal::presentation::Presentation;
use foreign_types::ForeignTypeRef;
use objc2::{
    MainThreadMarker, class, msg_send,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use serde::Serialize;
use tokio::sync::oneshot;

use crate::{
    presentation::{PresentationSession, PresentationTransition},
    viewports::PlatformViewportState,
};

/// `NSWindowOcclusionStateVisible`.
const OCCLUSION_STATE_VISIBLE: usize = 1 << 1;
const CG_IMAGE_ALPHA_INFO_MASK: u32 = 0x1f;
const CG_IMAGE_BYTE_ORDER_MASK: u32 = 0x7000;

type OcclusionStateFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize;

static ORIGINAL_OCCLUSION_STATE: OnceLock<OcclusionStateFn> = OnceLock::new();
static WINDOW_STATES: OnceLock<Mutex<HashMap<usize, PlatformViewportState>>> = OnceLock::new();
static PRESENTATION_SESSION: OnceLock<Mutex<PresentationSession>> = OnceLock::new();
static SPOOF_OCCLUSION: AtomicBool = AtomicBool::new(false);
/// Whether the app opted into the background-launch activation guard.
static BACKGROUND_LAUNCH: AtomicBool = AtomicBool::new(false);
/// Whether the current activation came from a user mouse interaction.
static USER_ACTIVATION: AtomicBool = AtomicBool::new(false);
/// Pid of the most recent frontmost application other than this process.
static LAST_OTHER_FRONTMOST_PID: AtomicI32 = AtomicI32::new(0);

const ACTIVATION_POLICY_REGULAR: i64 = 0;
const ACTIVATION_POLICY_ACCESSORY: i64 = 1;
const ACTIVATION_POLICY_PROHIBITED: i64 = 2;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetBitmapInfo(image: CGImageRef) -> u32;
}

/// Install the process-wide window hook once per process.
pub fn install_occlusion_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let _ = INSTALLED.get_or_init(|| {
        spoof_occlusion_state();
    });
}

pub fn platform_window_states() -> Vec<PlatformViewportState> {
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

/// Return the AppKit window number for a titled window recorded by the occlusion hook.
pub fn window_number_for_title(title: &str) -> Result<u32, String> {
    match recorded_window_number_for_title(title) {
        Ok(window_number) => Ok(window_number),
        Err(recorded_error) => window_number_from_window_server(title)
            .map_err(|window_server_error| format!("{recorded_error}; {window_server_error}")),
    }
}

fn recorded_window_number_for_title(title: &str) -> Result<u32, String> {
    let Some(states) = WINDOW_STATES.get() else {
        return Err("no macOS window state has been recorded yet".to_string());
    };
    let live_window_numbers = current_process_window_numbers();
    let mut states = states.lock().expect("platform window states lock poisoned");
    if let Ok(live_window_numbers) = &live_window_numbers {
        states.retain(|_, state| {
            state
                .window_number
                .is_none_or(|window_number| live_window_numbers.contains(&window_number))
        });
    }
    let window_numbers = states
        .values()
        .filter(|state| state.title.as_deref() == Some(title))
        .filter_map(|state| state.window_number)
        .collect::<Vec<_>>();
    match window_numbers.as_slice() {
        [window_number] => Ok(*window_number),
        [] => Err(format!("no recorded macOS window matched title {title:?}")),
        _ => Err(format!(
            "multiple recorded macOS windows matched title {title:?}"
        )),
    }
}

fn current_process_window_numbers() -> Result<HashSet<u32>, String> {
    let Some(window_info) = window::copy_window_info(kCGWindowListOptionAll, kCGNullWindowID)
    else {
        return Err("CoreGraphics returned no window metadata".to_string());
    };
    let pid = current_process_id_for_window_metadata()?;
    window_info
        .get_values(CFRange {
            location: 0,
            length: window_info.len(),
        })
        .into_iter()
        .filter_map(|value| unsafe {
            let info = WindowInfo::wrap_under_get_rule(value.cast());
            let owner_pid = window_info_number(&info, kCGWindowOwnerPID)?;
            (owner_pid == pid).then(|| window_info_number(&info, kCGWindowNumber))
        })
        .collect::<Option<HashSet<_>>>()
        .ok_or_else(|| "CoreGraphics window metadata was missing window numbers".to_string())
        .and_then(|window_numbers| {
            window_numbers
                .into_iter()
                .map(|window_number| {
                    u32::try_from(window_number).map_err(|error| {
                        format!("CoreGraphics window number was not a CGWindowID: {error}")
                    })
                })
                .collect()
        })
}

fn window_number_from_window_server(title: &str) -> Result<u32, String> {
    let Some(window_info) = window::copy_window_info(kCGWindowListOptionAll, kCGNullWindowID)
    else {
        return Err("CoreGraphics returned no window metadata".to_string());
    };
    let pid = current_process_id_for_window_metadata()?;
    let window_numbers = window_info
        .get_values(CFRange {
            location: 0,
            length: window_info.len(),
        })
        .into_iter()
        .filter_map(|value| unsafe {
            let info = WindowInfo::wrap_under_get_rule(value.cast());
            let owner_pid = window_info_number(&info, kCGWindowOwnerPID)?;
            let window_title = window_info_string(&info, kCGWindowName)?;
            (owner_pid == pid && window_title == title)
                .then(|| window_info_number(&info, kCGWindowNumber))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "CoreGraphics window metadata was missing window numbers".to_string())?;
    match window_numbers.as_slice() {
        [window_number] => u32::try_from(*window_number)
            .map_err(|error| format!("CoreGraphics window number was not a CGWindowID: {error}")),
        [] => Err(format!(
            "CoreGraphics found no current-process window titled {title:?}"
        )),
        _ => Err(format!(
            "CoreGraphics found multiple current-process windows titled {title:?}"
        )),
    }
}

type WindowInfo = CFDictionary<CFString, CFType>;

fn current_process_id_for_window_metadata() -> Result<i32, String> {
    i32::try_from(process::id())
        .map_err(|error| format!("process id did not fit CoreGraphics metadata: {error}"))
}

fn window_info_number(info: &WindowInfo, key: CFStringRef) -> Option<i32> {
    let key = unsafe { CFString::wrap_under_get_rule(key) };
    info.find(&key)
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|value| value.to_i32())
}

fn window_info_string(info: &WindowInfo, key: CFStringRef) -> Option<String> {
    let key = unsafe { CFString::wrap_under_get_rule(key) };
    info.find(&key)
        .and_then(|value| value.downcast::<CFString>())
        .map(|value| value.to_string())
}

/// Capture a window directly through Quartz and return an egui-compatible image.
pub fn capture_window_image(window_number: u32) -> Result<egui::ColorImage, String> {
    let image_options = kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution;
    let Some(image) = create_image(
        unsafe { CGRectNull },
        kCGWindowListOptionIncludingWindow,
        window_number as CGWindowID,
        image_options,
    ) else {
        return Err("CoreGraphics returned no window image".to_string());
    };
    color_image_from_cg_image(&image)
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
        spoofed_occlusion_state(real_state)
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
        window_number: unsafe { window_number(window) },
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

unsafe fn window_number(window: *mut AnyObject) -> Option<u32> {
    let number: isize = unsafe { msg_send![window, windowNumber] };
    u32::try_from(number).ok().filter(|number| *number > 0)
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

fn spoofed_occlusion_state(real_state: usize) -> usize {
    occlusion_state(real_state, SPOOF_OCCLUSION.load(Ordering::Acquire))
}

fn occlusion_state(real_state: usize, spoof: bool) -> usize {
    if spoof {
        OCCLUSION_STATE_VISIBLE
    } else {
        real_state
    }
}

fn presentation_session() -> &'static Mutex<PresentationSession> {
    PRESENTATION_SESSION.get_or_init(|| Mutex::new(PresentationSession::default()))
}

/// Apply one connection's presentation and return whether a live window needs a frame.
pub async fn configure_session(
    session_id: u64,
    presentation: Presentation,
) -> Result<bool, String> {
    install_occlusion_hook();
    run_on_main(move || {
        let observed_policy = activation_policy();
        let (previous, transition) = {
            let mut sessions = presentation_session()
                .lock()
                .expect("presentation session lock poisoned");
            let previous = sessions.clone();
            let transition = sessions.configure(
                session_id,
                presentation,
                observed_policy,
                ACTIVATION_POLICY_ACCESSORY,
            );
            (previous, transition)
        };
        if let Err(error) = apply_transition(transition) {
            *presentation_session()
                .lock()
                .expect("presentation session lock poisoned") = previous;
            return Err(error);
        }
        SPOOF_OCCLUSION.store(true, Ordering::Release);
        Ok::<_, String>(reevaluate_live_windows() > 0)
    })
    .await?
}

/// Remove one connection's presentation and restore the newest remaining request.
pub async fn disconnect_session(session_id: u64) -> Result<(), String> {
    run_on_main(move || {
        let observed_policy = activation_policy();
        let (previous, transition) = {
            let mut sessions = presentation_session()
                .lock()
                .expect("presentation session lock poisoned");
            let previous = sessions.clone();
            let transition =
                sessions.disconnect(session_id, observed_policy, ACTIVATION_POLICY_ACCESSORY);
            (previous, transition)
        };
        if let Some(transition) = transition
            && let Err(error) = apply_transition(transition)
        {
            *presentation_session()
                .lock()
                .expect("presentation session lock poisoned") = previous;
            return Err(error);
        }
        let active = presentation_session()
            .lock()
            .expect("presentation session lock poisoned")
            .is_active();
        SPOOF_OCCLUSION.store(active, Ordering::Release);
        reevaluate_live_windows();
        Ok::<_, String>(())
    })
    .await?
}

/// Enable the launch-to-exit activation guard for a background launch.
///
/// Call from the app before its event loop starts. While the session
/// presentation is background (the default), any activation of this app that
/// was not caused by a user mouse interaction is handed back to the
/// previously frontmost application in the same runloop turn, so instrumented
/// windows never take focus from the app the developer is working in. A
/// foreground presentation session suspends the guard; disconnecting resumes
/// it.
pub fn enable_background_launch_guard() {
    static OBSERVERS: OnceLock<()> = OnceLock::new();
    BACKGROUND_LAUNCH.store(true, Ordering::Release);
    let _ = OBSERVERS.get_or_init(|| {
        seed_frontmost_target();
        install_activation_observers();
    });
}

/// Return whether the activation guard currently applies.
fn activation_guard_enabled() -> bool {
    BACKGROUND_LAUNCH.load(Ordering::Acquire)
        && presentation_session()
            .lock()
            .expect("presentation session lock poisoned")
            .requested()
            == Presentation::Background
}

/// Record the frontmost application as the activation hand-back target.
fn seed_frontmost_target() {
    // SAFETY: NSWorkspace lookups are thread-safe reads.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return;
        }
        let frontmost: *mut AnyObject = msg_send![workspace, frontmostApplication];
        record_frontmost_target(frontmost);
    }
}

/// Remember `app` as the hand-back target when it is another process.
///
/// # Safety
///
/// `app` must be nil or a valid `NSRunningApplication`.
unsafe fn record_frontmost_target(app: *mut AnyObject) {
    if app.is_null() {
        return;
    }
    let pid: i32 = unsafe { msg_send![app, processIdentifier] };
    if pid > 0 && pid != process::id() as i32 {
        LAST_OTHER_FRONTMOST_PID.store(pid, Ordering::Release);
    }
}

/// Build an autoreleased `NSString` from a static C string.
///
/// # Safety
///
/// Standard AppKit string construction; safe for any valid C string.
unsafe fn nsstring(value: &'static CStr) -> *mut AnyObject {
    unsafe { msg_send![class!(NSString), stringWithUTF8String: value.as_ptr()] }
}

/// Register one block observer on a notification center.
///
/// # Safety
///
/// `center` must be a valid `NSNotificationCenter`. Registration passes a nil
/// queue, so the block runs on the posting thread.
unsafe fn add_observer(
    center: *mut AnyObject,
    name: &'static CStr,
    block: &RcBlock<dyn Fn(NonNull<AnyObject>)>,
) {
    unsafe {
        let name = nsstring(name);
        let _: *mut AnyObject = msg_send![
            center,
            addObserverForName: name,
            object: ptr::null_mut::<AnyObject>(),
            queue: ptr::null_mut::<AnyObject>(),
            usingBlock: &**block
        ];
    }
}

/// Install the workspace and application activation observers.
fn install_activation_observers() {
    let did_activate = RcBlock::new(|notification: NonNull<AnyObject>| {
        // SAFETY: the notification's application entry is an
        // NSRunningApplication; the notification is posted on the main thread.
        unsafe {
            let info: *mut AnyObject = msg_send![notification.as_ptr(), userInfo];
            if info.is_null() {
                return;
            }
            let key = nsstring(c"NSWorkspaceApplicationKey");
            let app: *mut AnyObject = msg_send![info, objectForKey: key];
            record_frontmost_target(app);
        }
    });
    let became_active = RcBlock::new(|_notification: NonNull<AnyObject>| {
        guard_became_active();
    });
    let resigned = RcBlock::new(|_notification: NonNull<AnyObject>| {
        USER_ACTIVATION.store(false, Ordering::Release);
    });

    // SAFETY: notification names are stable AppKit constants and both centers
    // are process singletons.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return;
        }
        let workspace_center: *mut AnyObject = msg_send![workspace, notificationCenter];
        add_observer(
            workspace_center,
            c"NSWorkspaceDidActivateApplicationNotification",
            &did_activate,
        );
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        add_observer(
            center,
            c"NSApplicationDidBecomeActiveNotification",
            &became_active,
        );
        add_observer(
            center,
            c"NSApplicationDidResignActiveNotification",
            &resigned,
        );
    }
}

/// Handle this app becoming active while the guard applies.
fn guard_became_active() {
    if !activation_guard_enabled() {
        return;
    }
    if activation_was_user_mouse() {
        USER_ACTIVATION.store(true, Ordering::Release);
        return;
    }
    hand_back_activation();
}

/// Return whether the in-flight event that caused activation was a mouse
/// interaction, which marks the activation as user-intended.
fn activation_was_user_mouse() -> bool {
    // SAFETY: this function is called only on the AppKit main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return false;
        }
        let event: *mut AnyObject = msg_send![app, currentEvent];
        if event.is_null() {
            return false;
        }
        let kind: usize = msg_send![event, type];
        // Left/right/other mouse down and up.
        matches!(kind, 1 | 2 | 3 | 4 | 25 | 26)
    }
}

/// Return whether this app is currently the active application.
fn application_is_active() -> bool {
    // SAFETY: this function is called only on the AppKit main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return false;
        }
        let active: bool = msg_send![app, isActive];
        active
    }
}

/// Hand activation to the remembered frontmost application, or deactivate.
fn hand_back_activation() {
    // SAFETY: this function is called only on the AppKit main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let mut yielded = false;
        let target_pid = LAST_OTHER_FRONTMOST_PID.load(Ordering::Acquire);
        if target_pid > 0 {
            let target: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: target_pid
            ];
            if !target.is_null() {
                let terminated: bool = msg_send![target, isTerminated];
                if !terminated {
                    let _: () = msg_send![app, yieldActivationToApplication: target];
                    let current: *mut AnyObject =
                        msg_send![class!(NSRunningApplication), currentApplication];
                    let cooperative = sel!(activateFromApplication:options:);
                    let responds: bool = msg_send![target, respondsToSelector: cooperative];
                    yielded = if responds {
                        msg_send![target, activateFromApplication: current, options: 0usize]
                    } else {
                        msg_send![target, activateWithOptions: 0usize]
                    };
                }
            }
        }
        if !yielded {
            let _: () = msg_send![app, deactivate];
        }
    }
}

/// Reassert background presentation after an app frame if the app changed it.
pub fn reassert_background_policy() -> Option<PolicyConflict> {
    let _mtm = MainThreadMarker::new()?;
    if activation_guard_enabled()
        && !USER_ACTIVATION.load(Ordering::Acquire)
        && application_is_active()
    {
        // A hand-back denied inside the activation notification is retried
        // here once per frame until the app has genuinely ceded activation.
        hand_back_activation();
    }
    let observed_policy = activation_policy();
    let mut session = presentation_session()
        .lock()
        .expect("presentation session lock poisoned");
    if !session.should_reassert(observed_policy, ACTIVATION_POLICY_ACCESSORY) {
        return None;
    }
    let first_conflict = session.report_conflict(observed_policy, ACTIVATION_POLICY_ACCESSORY);
    drop(session);
    if observed_policy != Some(ACTIVATION_POLICY_ACCESSORY) {
        let _ = set_activation_policy(ACTIVATION_POLICY_ACCESSORY);
        deactivate_application();
    }
    first_conflict.then_some(PolicyConflict {
        observed_activation_policy: observed_policy.map(activation_policy_name),
        requested_presentation: Presentation::Background,
    })
}

/// Structured diagnostic emitted when app code conflicts with background mode.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyConflict {
    requested_presentation: Presentation,
    observed_activation_policy: Option<String>,
}

fn apply_transition(transition: PresentationTransition) -> Result<(), String> {
    if let Some(policy) = transition.target_activation_policy
        && !set_activation_policy(policy)
    {
        return Err(format!("NSApplication rejected activation policy {policy}"));
    }
    if transition.deactivate {
        deactivate_application();
    }
    Ok(())
}

fn activation_policy() -> Option<i64> {
    // SAFETY: this function is called only on the AppKit main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return None;
        }
        let policy: isize = msg_send![app, activationPolicy];
        Some(policy as i64)
    }
}

fn set_activation_policy(policy: i64) -> bool {
    // SAFETY: this function is called only on the AppKit main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return false;
        }
        let policy = policy as isize;
        let applied: bool = msg_send![app, setActivationPolicy: policy];
        applied
    }
}

fn deactivate_application() {
    // SAFETY: this function is called only on the AppKit main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            let _: () = msg_send![app, deactivate];
        }
    }
}

fn reevaluate_live_windows() -> usize {
    // SAFETY: this function is called only on the AppKit main thread. The
    // delegate selector is checked before it is invoked.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return 0;
        }
        let windows: *mut AnyObject = msg_send![app, windows];
        if windows.is_null() {
            return 0;
        }
        let count: usize = msg_send![windows, count];
        let mut reevaluated = 0;
        for index in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: index];
            if window.is_null() {
                continue;
            }
            let delegate: *mut AnyObject = msg_send![window, delegate];
            if delegate.is_null() {
                continue;
            }
            let selector = sel!(windowDidChangeOcclusionState:);
            let responds: bool = msg_send![delegate, respondsToSelector: selector];
            if responds {
                reevaluated += 1;
                let _: () = msg_send![delegate, windowDidChangeOcclusionState: ptr::null_mut::<AnyObject>()];
            }
        }
        reevaluated
    }
}

fn activation_policy_name(policy: i64) -> String {
    match policy {
        ACTIVATION_POLICY_REGULAR => "regular".to_string(),
        ACTIVATION_POLICY_ACCESSORY => "accessory".to_string(),
        ACTIVATION_POLICY_PROHIBITED => "prohibited".to_string(),
        other => format!("unknown({other})"),
    }
}

#[cfg(test)]
fn bundle_root(executable: &Path) -> Option<String> {
    let mut root = PathBuf::new();
    for component in executable.components() {
        root.push(component.as_os_str());
        if component.as_os_str().to_string_lossy().ends_with(".app") {
            return Some(root.display().to_string());
        }
    }
    None
}

async fn run_on_main<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if MainThreadMarker::new().is_some() {
        return Ok(operation());
    }

    let (sender, receiver) = oneshot::channel();
    DispatchQueue::main().exec_async(move || {
        drop(sender.send(operation()));
    });
    receiver
        .await
        .map_err(|_| "macOS main-thread operation was cancelled".to_string())
}

fn color_image_from_cg_image(image: &CGImage) -> Result<egui::ColorImage, String> {
    let width = image.width();
    let height = image.height();
    let bytes_per_row = image.bytes_per_row();
    if width == 0 || height == 0 {
        return Err("CoreGraphics returned an empty window image".to_string());
    }
    validate_cg_image_format(image.bits_per_component(), image.bits_per_pixel(), unsafe {
        CGImageGetBitmapInfo(image.as_ptr())
    })?;
    let min_row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| "CoreGraphics image row width overflowed".to_string())?;
    if bytes_per_row < min_row_bytes {
        return Err(format!(
            "CoreGraphics row stride {bytes_per_row} is too small for {width} pixels"
        ));
    }
    let data = image.data();
    color_image_from_bgra_rows(width, height, bytes_per_row, data.bytes())
}

fn validate_cg_image_format(
    bits_per_component: usize,
    bits_per_pixel: usize,
    bitmap_info: u32,
) -> Result<(), String> {
    if bits_per_component != 8 || bits_per_pixel != 32 {
        return Err(format!(
            "unsupported CoreGraphics image format: {bits_per_component} bits/component, \
             {bits_per_pixel} bits/pixel"
        ));
    }
    let alpha_info = bitmap_info & CG_IMAGE_ALPHA_INFO_MASK;
    let byte_order = bitmap_info & CG_IMAGE_BYTE_ORDER_MASK;
    if alpha_info != kCGImageAlphaPremultipliedFirst || byte_order != kCGBitmapByteOrder32Little {
        return Err(format!(
            "unsupported CoreGraphics image format: bitmap info 0x{bitmap_info:x}"
        ));
    }
    Ok(())
}

fn color_image_from_bgra_rows(
    width: usize,
    height: usize,
    bytes_per_row: usize,
    data: &[u8],
) -> Result<egui::ColorImage, String> {
    let last_row_offset = height
        .checked_sub(1)
        .and_then(|row| row.checked_mul(bytes_per_row))
        .ok_or_else(|| "CoreGraphics image size overflowed".to_string())?;
    let required_bytes = last_row_offset
        .checked_add(width.saturating_mul(4))
        .ok_or_else(|| "CoreGraphics image byte count overflowed".to_string())?;
    if data.len() < required_bytes {
        return Err(format!(
            "CoreGraphics image data is truncated: {} bytes for {required_bytes} required",
            data.len()
        ));
    }

    let mut rgba = Vec::with_capacity(
        width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "CoreGraphics image pixel count overflowed".to_string())?,
    );
    let mut saw_visible_pixel = false;
    for row in data.chunks(bytes_per_row).take(height) {
        for pixel in row[..width * 4].chunks_exact(4) {
            let blue = pixel[0];
            let green = pixel[1];
            let red = pixel[2];
            let alpha = pixel[3];
            saw_visible_pixel |= alpha != 0;
            rgba.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    if !saw_visible_pixel {
        return Err(
            "CoreGraphics returned only transparent pixels; Screen Recording permission may be \
             missing"
                .to_string(),
        );
    }

    Ok(egui::ColorImage::from_rgba_premultiplied(
        [width, height],
        &rgba,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_image_from_bgra_rows_converts_to_rgba() {
        let image =
            color_image_from_bgra_rows(2, 1, 12, &[3, 2, 1, 255, 30, 20, 10, 128, 0, 0, 0, 0])
                .expect("image");

        assert_eq!(image.size, [2, 1]);
        assert_eq!(
            image.pixels[0],
            egui::Color32::from_rgba_premultiplied(1, 2, 3, 255)
        );
        assert_eq!(
            image.pixels[1],
            egui::Color32::from_rgba_premultiplied(10, 20, 30, 128)
        );
    }

    #[test]
    fn validate_cg_image_format_rejects_unexpected_bitmap_info() {
        let expected_info = kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little;

        assert!(validate_cg_image_format(8, 32, expected_info).is_ok());
        assert!(validate_cg_image_format(16, 32, expected_info).is_err());
        assert!(validate_cg_image_format(8, 32, kCGImageAlphaPremultipliedFirst).is_err());
    }

    #[test]
    fn occlusion_state_is_real_until_a_session_enables_spoofing() {
        assert_eq!(occlusion_state(0, false), 0);
        assert_eq!(
            occlusion_state(OCCLUSION_STATE_VISIBLE, false),
            OCCLUSION_STATE_VISIBLE
        );
        assert_eq!(occlusion_state(0, true), OCCLUSION_STATE_VISIBLE);
    }

    #[test]
    fn status_helpers_use_stable_diagnostic_values() {
        assert_eq!(activation_policy_name(ACTIVATION_POLICY_REGULAR), "regular");
        assert_eq!(
            activation_policy_name(ACTIVATION_POLICY_ACCESSORY),
            "accessory"
        );
        assert_eq!(bundle_root(Path::new("bin/example")), None);
        assert_eq!(
            bundle_root(Path::new("build/example.app/Contents/MacOS/example")),
            Some("build/example.app".to_string())
        );
    }
}
