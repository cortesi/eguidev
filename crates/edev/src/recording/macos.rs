//! macOS ScreenCaptureKit recording backend.

use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{Arc, Mutex, mpsc},
};

use block2::RcBlock;
use libproc::processes::{self, ProcFilter};
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{NSObjectProtocol, ProtocolObject},
};
use objc2_av_foundation::{AVFileTypeQuickTimeMovie, AVVideoCodecTypeH264};
use objc2_foundation::{
    NSArray, NSError, NSObject, NSOperatingSystemVersion, NSProcessInfo, NSURL,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCRecordingOutput, SCRecordingOutputConfiguration,
    SCRecordingOutputDelegate, SCRunningApplication, SCShareableContent, SCStream,
    SCStreamConfiguration, SCWindow,
};

use super::{RecordingRequest, RecordingSummary, WindowCandidate, select_window};
use crate::EdevError;

/// Minimum macOS version with ScreenCaptureKit direct recording output.
const MIN_MACOS_VERSION: NSOperatingSystemVersion = NSOperatingSystemVersion {
    majorVersion: 15,
    minorVersion: 0,
    patchVersion: 0,
};

/// Delegate state shared with ScreenCaptureKit callbacks.
#[derive(Default)]
struct RecordingDelegateIvars {
    /// Ordered recording events observed by the delegate.
    events: Arc<Mutex<Vec<RecordingEvent>>>,
}

/// Recording events observed from ScreenCaptureKit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordingEvent {
    /// Native recording started.
    Started,
    /// Native recording failed.
    Failed(String),
    /// Native recording finished.
    Finished,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = RecordingDelegateIvars]
    struct RecordingDelegate;

    unsafe impl NSObjectProtocol for RecordingDelegate {}

    unsafe impl SCRecordingOutputDelegate for RecordingDelegate {
        #[unsafe(method(recordingOutputDidStartRecording:))]
        fn recording_started(&self, _recording_output: &SCRecordingOutput) {
            self.events()
                .lock()
                .expect("events lock")
                .push(RecordingEvent::Started);
        }

        #[unsafe(method(recordingOutput:didFailWithError:))]
        fn recording_failed(&self, _recording_output: &SCRecordingOutput, error: &NSError) {
            self.events()
                .lock()
                .expect("events lock")
                .push(RecordingEvent::Failed(error.to_string()));
        }

        #[unsafe(method(recordingOutputDidFinishRecording:))]
        fn recording_finished(&self, _recording_output: &SCRecordingOutput) {
            self.events()
                .lock()
                .expect("events lock")
                .push(RecordingEvent::Finished);
        }
    }
);

impl RecordingDelegate {
    /// Create a delegate retaining the shared event log.
    fn new(events: Arc<Mutex<Vec<RecordingEvent>>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(RecordingDelegateIvars { events });
        unsafe { msg_send![super(this), init] }
    }

    /// Return the delegate event log.
    fn events(&self) -> &Arc<Mutex<Vec<RecordingEvent>>> {
        &self.ivars().events
    }
}

/// Active native recording session.
pub struct NativeRecording {
    /// Final output path.
    outfile: PathBuf,
    /// ScreenCaptureKit capture stream.
    stream: Retained<SCStream>,
    /// ScreenCaptureKit file recorder.
    recording: Retained<SCRecordingOutput>,
    /// Delegate retained for the whole recording.
    _delegate: Retained<RecordingDelegate>,
    /// Delegate events retained for final error reporting.
    events: Arc<Mutex<Vec<RecordingEvent>>>,
}

/// Confirm that native recording is supported on this compiled target.
pub fn ensure_supported() -> Result<(), EdevError> {
    let process_info = NSProcessInfo::processInfo();
    if process_info.isOperatingSystemAtLeastVersion(MIN_MACOS_VERSION) {
        return Ok(());
    }

    let current = process_info.operatingSystemVersion();
    Err(EdevError::RecordFailed(format!(
        "`edev record` requires macOS 15.0 or newer; current macOS is {}.{}.{}",
        current.majorVersion, current.minorVersion, current.patchVersion
    )))
}

impl NativeRecording {
    /// Stop and finalize the recording file.
    pub(crate) fn stop(self) -> Result<RecordingSummary, EdevError> {
        let Self {
            outfile,
            stream,
            recording,
            _delegate: delegate,
            events,
        } = self;
        let stop_result = complete_stream_action("stop recording capture", |block| unsafe {
            stream.stopCaptureWithCompletionHandler(Some(block));
        });
        let remove_result = unsafe { stream.removeRecordingOutput_error(&recording) }
            .map_err(|error| recording_error(&error));

        stop_result?;
        remove_result?;
        if let Some(error) = delegate_failure(&events) {
            return Err(error);
        }

        let native_size = unsafe { recording.recordedFileSize() }.max(0) as u64;
        drop(stream);
        drop(recording);
        drop(delegate);
        let file_size = native_size.max(
            fs::metadata(&outfile)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        );
        if file_size == 0 {
            return Err(EdevError::RecordFailed(format!(
                "recording finalized but wrote no bytes to {}",
                outfile.display()
            )));
        }

        Ok(RecordingSummary { outfile, file_size })
    }
}

/// Return process ids in the launched app's process group.
pub fn process_group_members(process_group_id: Option<i32>) -> Vec<i32> {
    let Some(process_group_id) = process_group_id else {
        return Vec::new();
    };
    let mut pids = live_process_group_members(process_group_id);
    pids.push(process_group_id);
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Return only currently live process ids in one process group.
pub fn live_process_group_members(process_group_id: i32) -> Vec<i32> {
    let Ok(pgrpid) = u32::try_from(process_group_id) else {
        return Vec::new();
    };
    processes::pids_by_type(ProcFilter::ByProgramGroup { pgrpid })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|pid| i32::try_from(pid).ok())
        .collect::<Vec<_>>()
}

/// Start a native recording session.
pub fn start(request: &RecordingRequest) -> Result<NativeRecording, EdevError> {
    let content = shareable_content()?;
    let windows = unsafe { content.windows() }.to_vec();
    let window_candidates = windows
        .iter()
        .filter(|window| unsafe { window.isOnScreen() })
        .map(|window| window_candidate(window))
        .collect::<Vec<_>>();
    let selection = select_window(&window_candidates, &request.title, &request.app_process_ids)?;
    let window = windows
        .into_iter()
        .find(|window| unsafe { window.windowID() } == selection.window_id)
        .ok_or_else(|| {
            EdevError::RecordFailed(format!(
                "selected native window {} disappeared before recording started",
                selection.window_id
            ))
        })?;
    let display = choose_display(&content, &window)?;
    let recording = start_window_recording(&request.outfile, &window, &display)?;
    if !selection.owner_match {
        eprintln!(
            "edev: recording window matched by title only; app process tree did not own {:?}",
            request.title
        );
    }
    Ok(recording)
}

/// Convert one ScreenCaptureKit window into the pure selection shape.
fn window_candidate(window: &SCWindow) -> WindowCandidate {
    let app = unsafe { window.owningApplication() };
    WindowCandidate {
        window_id: unsafe { window.windowID() },
        title: unsafe { window.title() }.map(|title| title.to_string()),
        owner_name: app.as_ref().map(application_name),
        process_id: app.as_ref().and_then(application_process_id),
    }
}

/// Return a running application's display name.
fn application_name(app: &Retained<SCRunningApplication>) -> String {
    unsafe { app.applicationName() }.to_string()
}

/// Return a running application's process id.
fn application_process_id(app: &Retained<SCRunningApplication>) -> Option<i32> {
    Some(unsafe { app.processID() })
}

/// Choose the display with the largest intersection with the target window.
fn choose_display(
    content: &SCShareableContent,
    window: &SCWindow,
) -> Result<Retained<SCDisplay>, EdevError> {
    let displays = unsafe { content.displays() }.to_vec();
    let window_frame = unsafe { window.frame() };
    let intersection_area = |display: &SCDisplay| {
        let display_frame = unsafe { display.frame() };
        let left = display_frame.origin.x.max(window_frame.origin.x);
        let right = (display_frame.origin.x + display_frame.size.width)
            .min(window_frame.origin.x + window_frame.size.width);
        let top = display_frame.origin.y.max(window_frame.origin.y);
        let bottom = (display_frame.origin.y + display_frame.size.height)
            .min(window_frame.origin.y + window_frame.size.height);
        (right - left).max(0.0) * (bottom - top).max(0.0)
    };
    displays
        .into_iter()
        .max_by(|left, right| {
            intersection_area(left)
                .partial_cmp(&intersection_area(right))
                .unwrap_or(Ordering::Equal)
        })
        .ok_or_else(|| EdevError::RecordFailed("ScreenCaptureKit returned no displays".to_string()))
}

/// Start ScreenCaptureKit recording for one window on one display.
fn start_window_recording(
    outfile: &Path,
    window: &SCWindow,
    display: &SCDisplay,
) -> Result<NativeRecording, EdevError> {
    let window_frame = unsafe { window.frame() };
    let events = Arc::new(Mutex::new(Vec::new()));
    let delegate = RecordingDelegate::new(Arc::clone(&events));
    let delegate_protocol: &ProtocolObject<dyn SCRecordingOutputDelegate> =
        ProtocolObject::from_ref(&*delegate);
    let url = NSURL::from_file_path(outfile).ok_or_else(|| {
        EdevError::RecordFailed(format!(
            "invalid recording output path: {}",
            outfile.display()
        ))
    })?;

    let recording_config = unsafe {
        let config = SCRecordingOutputConfiguration::new();
        config.setOutputURL(&url);
        config.setVideoCodecType(AVVideoCodecTypeH264.expect("H.264 codec constant"));
        config.setOutputFileType(AVFileTypeQuickTimeMovie.expect("QuickTime movie constant"));
        config
    };
    let recording = unsafe {
        SCRecordingOutput::initWithConfiguration_delegate(
            SCRecordingOutput::alloc(),
            &recording_config,
            delegate_protocol,
        )
    };
    let included_windows = NSArray::from_slice(&[window]);
    let filter = unsafe {
        SCContentFilter::initWithDisplay_includingWindows(
            SCContentFilter::alloc(),
            display,
            &included_windows,
        )
    };
    let point_pixel_scale = unsafe { filter.pointPixelScale() } as f64;
    let width = native_pixel_dimension(window_frame.size.width, point_pixel_scale);
    let height = native_pixel_dimension(window_frame.size.height, point_pixel_scale);
    let stream_config = unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width);
        config.setHeight(height);
        config.setSourceRect(window_frame);
        config.setCapturesAudio(false);
        config.setCapturesShadowsOnly(false);
        config
    };
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &stream_config,
            None,
        )
    };

    unsafe { stream.addRecordingOutput_error(&recording) }
        .map_err(|error| recording_error(&error))?;
    if let Err(error) = complete_stream_action("start recording capture", |block| unsafe {
        stream.startCaptureWithCompletionHandler(Some(block));
    }) {
        let _remove_result = unsafe { stream.removeRecordingOutput_error(&recording) };
        return Err(error);
    }
    if let Some(error) = delegate_failure(&events) {
        let _stop_result =
            complete_stream_action("stop failed recording capture", |block| unsafe {
                stream.stopCaptureWithCompletionHandler(Some(block));
            });
        let _remove_result = unsafe { stream.removeRecordingOutput_error(&recording) };
        return Err(error);
    }

    Ok(NativeRecording {
        outfile: outfile.to_path_buf(),
        stream,
        recording,
        _delegate: delegate,
        events,
    })
}

/// Retrieve current ScreenCaptureKit content through its completion callback.
fn shareable_content() -> Result<Retained<SCShareableContent>, EdevError> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if let Some(error) = NonNull::new(error) {
                Err(format!(
                    "ScreenCaptureKit window enumeration failed: {}",
                    unsafe { error.as_ref() }
                ))
            } else {
                unsafe { Retained::retain(content) }.ok_or_else(|| {
                    "ScreenCaptureKit returned no shareable window content".to_string()
                })
            };
            let _send = tx.send(result);
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &block,
        );
    }
    rx.recv()
        .map_err(|error| {
            EdevError::RecordFailed(format!(
                "ScreenCaptureKit enumeration callback was not delivered: {error}"
            ))
        })?
        .map_err(EdevError::RecordFailed)
}

/// Run a ScreenCaptureKit start/stop action and wait for its completion
/// callback.
fn complete_stream_action(
    name: &'static str,
    action: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), EdevError> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        let result = if let Some(error) = NonNull::new(error) {
            Err(format!("{name} failed: {}", unsafe { error.as_ref() }))
        } else {
            Ok(())
        };
        let _send = tx.send(result);
    });
    action(&block);
    rx.recv()
        .map_err(|error| {
            EdevError::RecordFailed(format!("{name} callback was not delivered: {error}"))
        })?
        .map_err(EdevError::RecordFailed)
}

/// Convert a ScreenCaptureKit point dimension to a non-zero pixel dimension.
fn native_pixel_dimension(points: f64, point_pixel_scale: f64) -> usize {
    (points.max(1.0) * point_pixel_scale.max(1.0)).round() as usize
}

/// Convert a retained NSError into an edev recording error.
fn recording_error(error: &NSError) -> EdevError {
    EdevError::RecordFailed(error.to_string())
}

/// Return the first native delegate failure observed so far.
fn delegate_failure(events: &Arc<Mutex<Vec<RecordingEvent>>>) -> Option<EdevError> {
    let events = events.lock().ok()?;
    events.iter().find_map(|event| match event {
        RecordingEvent::Failed(message) => Some(EdevError::RecordFailed(message.clone())),
        RecordingEvent::Started | RecordingEvent::Finished => None,
    })
}
