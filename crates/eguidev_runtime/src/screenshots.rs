//! Screenshot state tracking and management.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tokio::sync::Notify;

use crate::{
    registry::{lock, viewport_id_to_string},
    types::Rect,
};

/// The state of one screenshot request, from queueing to its answer.
#[derive(Debug, Clone)]
pub struct ScreenshotState {
    /// What the request captures.
    pub kind: ScreenshotKind,
    /// The captured image, once the callback delivers it.
    image: Option<Arc<egui::ColorImage>>,
    /// The frame count of the viewport frame that sent the command to egui.
    sent_frame: Option<u64>,
    /// Whether egui dropped the callback without an image.
    dropped: bool,
    /// Wakes the waiter when the request is answered or dropped.
    notify: Arc<Notify>,
}

impl ScreenshotState {
    pub fn pending(kind: ScreenshotKind) -> Self {
        Self {
            kind,
            image: None,
            sent_frame: None,
            dropped: false,
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn mark_ready(&mut self, image: Arc<egui::ColorImage>) {
        self.image = Some(image);
        self.notify.notify_waiters();
    }

    fn mark_dropped(&mut self) {
        self.dropped = true;
        self.notify.notify_waiters();
    }

    pub fn is_ready(&self) -> bool {
        self.image.is_some()
    }

    /// Whether egui dropped the callback without an image, for example
    /// because the viewport closed before it painted.
    pub fn is_dropped(&self) -> bool {
        self.dropped
    }

    /// The frame count of the viewport frame that sent the command, once it
    /// is sent.
    pub fn sent_frame(&self) -> Option<u64> {
        self.sent_frame
    }

    pub fn image(&self) -> Option<Arc<egui::ColorImage>> {
        self.image.clone()
    }

    pub fn notify(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }
}

#[derive(Debug, Clone)]
pub enum ScreenshotKind {
    Viewport,
    Widget { rect: Rect, pixels_per_point: f32 },
}

pub fn screenshot_kind_label(kind: &ScreenshotKind) -> String {
    match kind {
        ScreenshotKind::Viewport => "viewport".to_string(),
        ScreenshotKind::Widget { .. } => "widget".to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotDebugRequest {
    pub request_id: u64,
    pub viewport_id: String,
    pub kind: String,
    pub frame_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotDebugCommand {
    pub request_id: Option<u64>,
    pub viewport_id: String,
    pub frame_count: u64,
}

/// The last answer of a screenshot callback.
#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotDebugCallback {
    /// The request that the callback belongs to.
    pub request_id: u64,
    /// The viewport that the request targets.
    pub viewport_id: String,
    /// Whether the request was still pending when the callback answered.
    pub matched: bool,
    /// The image size, or `None` when egui dropped the callback without an
    /// image.
    pub image_size: Option<[usize; 2]>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScreenshotDebugState {
    /// Requests that a capture queued.
    pub requests_queued: u64,
    /// Commands that a viewport frame sent to egui.
    pub commands_sent: u64,
    /// Callbacks that delivered an image.
    pub images_received: u64,
    /// Images that arrived after their request was released.
    pub images_unknown_request: u64,
    /// Callbacks that egui dropped without an image.
    pub callbacks_dropped: u64,
    pub last_request: Option<ScreenshotDebugRequest>,
    pub last_command: Option<ScreenshotDebugCommand>,
    pub last_callback: Option<ScreenshotDebugCallback>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotDebugRequestInfo {
    pub request_id: u64,
    pub kind: String,
    pub ready: bool,
    pub sent_frame: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenshotDebugSnapshot {
    pub enabled: bool,
    pub frame_count: u64,
    pub pending_requests: Vec<ScreenshotDebugRequestInfo>,
    pub debug: ScreenshotDebugState,
}

/// The screenshot requests and their debug state. Clones share the state, so
/// the callback of each request can mark it ready from the render thread.
#[derive(Debug, Clone)]
pub struct ScreenshotManager {
    screenshots: Arc<Mutex<HashMap<u64, ScreenshotState>>>,
    screenshot_debug: Arc<Mutex<ScreenshotDebugState>>,
    /// The requests of each viewport whose callbacks wait for their command
    /// to be sent, oldest first.
    queued: Arc<Mutex<HashMap<egui::ViewportId, VecDeque<u64>>>>,
}

/// Owns the answer of one screenshot callback. egui drops the callback
/// without calling it when the viewport goes away before it paints, and the
/// drop then reports the request as dropped.
struct CallbackGuard {
    manager: ScreenshotManager,
    request_id: u64,
    viewport_id: egui::ViewportId,
    enabled: bool,
    answered: bool,
}

impl CallbackGuard {
    fn complete(mut self, image: &Arc<egui::ColorImage>) {
        self.answered = true;
        self.manager.receive_screenshot(
            self.request_id,
            self.viewport_id,
            Some(image),
            self.enabled,
        );
    }
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        if !self.answered {
            self.manager
                .receive_screenshot(self.request_id, self.viewport_id, None, self.enabled);
        }
    }
}

impl ScreenshotManager {
    pub(crate) fn new() -> Self {
        Self {
            screenshots: Arc::new(Mutex::new(HashMap::new())),
            screenshot_debug: Arc::new(Mutex::new(ScreenshotDebugState::default())),
            queued: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Take the oldest request of `viewport_id` whose command waits to be
    /// sent.
    fn take_queued(&self, viewport_id: egui::ViewportId) -> Option<u64> {
        let mut queued = lock(&self.queued, "screenshot queue lock");
        let requests = queued.get_mut(&viewport_id)?;
        let request_id = requests.pop_front();
        if requests.is_empty() {
            queued.remove(&viewport_id);
        }
        request_id
    }

    /// The callback of screenshot request `request_id` of `viewport_id`. It
    /// marks the request ready when the image arrives, and dropped when egui
    /// discards it unanswered.
    pub(crate) fn callback(
        &self,
        request_id: u64,
        viewport_id: egui::ViewportId,
        enabled: bool,
    ) -> egui::ScreenshotCallback {
        lock(&self.queued, "screenshot queue lock")
            .entry(viewport_id)
            .or_default()
            .push_back(request_id);
        let guard = CallbackGuard {
            manager: self.clone(),
            request_id,
            viewport_id,
            enabled,
            answered: false,
        };
        egui::ScreenshotCallback::new(move |image| guard.complete(&image))
    }

    pub(crate) fn screenshot_state(&self, request_id: u64) -> Option<ScreenshotState> {
        lock(&self.screenshots, "screenshots lock")
            .get(&request_id)
            .cloned()
    }

    pub(crate) fn insert_screenshot(&self, request_id: u64, state: ScreenshotState) {
        let mut screenshots = lock(&self.screenshots, "screenshots lock");
        screenshots.insert(request_id, state);
    }

    pub(crate) fn take_screenshot(&self, request_id: u64) -> Option<ScreenshotState> {
        let mut screenshots = lock(&self.screenshots, "screenshots lock");
        screenshots.remove(&request_id)
    }

    /// Record the answer of the callback of `request_id`: an image, or `None`
    /// when egui dropped the callback.
    fn receive_screenshot(
        &self,
        request_id: u64,
        viewport_id: egui::ViewportId,
        image: Option<&Arc<egui::ColorImage>>,
        enabled: bool,
    ) {
        let matched = {
            let mut screenshots = lock(&self.screenshots, "screenshots lock");
            match (screenshots.get_mut(&request_id), image) {
                (Some(state), Some(image)) => {
                    state.mark_ready(Arc::clone(image));
                    true
                }
                (Some(state), None) => {
                    state.mark_dropped();
                    true
                }
                (None, _) => false,
            }
        };
        let mut debug = lock(&self.screenshot_debug, "screenshot debug lock");
        match image {
            Some(_) if matched => debug.images_received += 1,
            Some(_) => debug.images_unknown_request += 1,
            None => debug.callbacks_dropped += 1,
        }
        let image_size = image.map(|image| image.size);
        self.log_screenshot(
            enabled,
            format!(
                "callback request_id={request_id} viewport={} matched={matched} size={image_size:?}",
                viewport_id_to_string(viewport_id),
            ),
        );
        debug.last_callback = Some(ScreenshotDebugCallback {
            request_id,
            viewport_id: viewport_id_to_string(viewport_id),
            matched,
            image_size,
        });
    }

    pub(crate) fn record_screenshot_request(
        &self,
        request_id: u64,
        viewport_id: egui::ViewportId,
        kind: &ScreenshotKind,
        enabled: bool,
        frame_count: u64,
    ) {
        let mut debug = lock(&self.screenshot_debug, "screenshot debug lock");
        debug.requests_queued += 1;
        debug.last_request = Some(ScreenshotDebugRequest {
            request_id,
            viewport_id: viewport_id_to_string(viewport_id),
            kind: screenshot_kind_label(kind),
            frame_count,
        });
        self.log_screenshot(
            enabled,
            format!(
                "request queued id={request_id} viewport={} kind={} frame={frame_count}",
                viewport_id_to_string(viewport_id),
                screenshot_kind_label(kind),
            ),
        );
    }

    /// Record that the frame `frame_count` of `viewport_id` sent a screenshot
    /// command to egui, and return the request of that command.
    pub(crate) fn record_screenshot_command_sent(
        &self,
        viewport_id: egui::ViewportId,
        enabled: bool,
        frame_count: u64,
    ) -> Option<u64> {
        let request_id = self.take_queued(viewport_id);
        if let Some(request_id) = request_id
            && let Some(state) = lock(&self.screenshots, "screenshots lock").get_mut(&request_id)
        {
            state.sent_frame = Some(frame_count);
        }
        let mut debug = lock(&self.screenshot_debug, "screenshot debug lock");
        debug.commands_sent += 1;
        debug.last_command = Some(ScreenshotDebugCommand {
            request_id,
            viewport_id: viewport_id_to_string(viewport_id),
            frame_count,
        });
        self.log_screenshot(
            enabled,
            format!(
                "command sent request_id={request_id:?} viewport={} frame={frame_count}",
                viewport_id_to_string(viewport_id),
            ),
        );
        request_id
    }

    pub(crate) fn screenshot_debug_snapshot(
        &self,
        enabled: bool,
        frame_count: u64,
    ) -> ScreenshotDebugSnapshot {
        let screenshots = lock(&self.screenshots, "screenshots lock");
        let pending_requests = screenshots
            .iter()
            .map(|(request_id, state)| ScreenshotDebugRequestInfo {
                request_id: *request_id,
                kind: screenshot_kind_label(&state.kind),
                ready: state.is_ready(),
                sent_frame: state.sent_frame,
            })
            .collect::<Vec<_>>();
        let debug = lock(&self.screenshot_debug, "screenshot debug lock").clone();
        ScreenshotDebugSnapshot {
            enabled,
            frame_count,
            pending_requests,
            debug,
        }
    }

    pub(crate) fn log_screenshot(&self, enabled: bool, line: impl AsRef<str>) {
        if !enabled {
            return;
        }
        eprintln!("eguidev: screenshot {}", line.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn white_pixel() -> Arc<egui::ColorImage> {
        Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]))
    }

    #[test]
    fn a_screenshot_callback_marks_its_request_ready() {
        let manager = ScreenshotManager::new();
        let request_id = 42_u64;
        manager.insert_screenshot(
            request_id,
            ScreenshotState::pending(ScreenshotKind::Viewport),
        );

        manager
            .callback(request_id, egui::ViewportId::ROOT, true)
            .complete(white_pixel());

        let state = manager
            .screenshot_state(request_id)
            .expect("screenshot state");
        assert!(state.is_ready());
        assert!(!state.is_dropped());

        let snapshot = manager.screenshot_debug_snapshot(true, 0);
        assert_eq!(snapshot.debug.images_received, 1);
        assert_eq!(snapshot.debug.images_unknown_request, 0);
        assert_eq!(snapshot.debug.callbacks_dropped, 0);
        let last = snapshot.debug.last_callback.expect("last callback");
        assert_eq!(last.request_id, request_id);
        assert!(last.matched);
        assert_eq!(last.image_size, Some([1, 1]));
    }

    #[test]
    fn a_screenshot_callback_of_an_unknown_request_is_recorded() {
        let manager = ScreenshotManager::new();
        manager
            .callback(7, egui::ViewportId::ROOT, true)
            .complete(white_pixel());

        let snapshot = manager.screenshot_debug_snapshot(true, 0);
        assert_eq!(snapshot.debug.images_unknown_request, 1);
        assert_eq!(snapshot.debug.images_received, 0);
        assert!(!snapshot.debug.last_callback.expect("last callback").matched);
    }

    #[test]
    fn a_dropped_screenshot_callback_marks_its_request_dropped() {
        let manager = ScreenshotManager::new();
        manager.insert_screenshot(3, ScreenshotState::pending(ScreenshotKind::Viewport));

        let callback = manager.callback(3, egui::ViewportId::ROOT, true);
        let clone = callback.clone();
        drop(callback);
        assert!(
            !manager.screenshot_state(3).expect("state").is_dropped(),
            "a live clone keeps the callback"
        );
        drop(clone);

        let state = manager.screenshot_state(3).expect("state");
        assert!(state.is_dropped());
        assert!(!state.is_ready());
        let snapshot = manager.screenshot_debug_snapshot(true, 0);
        assert_eq!(snapshot.debug.callbacks_dropped, 1);
        assert_eq!(
            snapshot
                .debug
                .last_callback
                .expect("last callback")
                .image_size,
            None
        );
    }

    #[test]
    fn a_completed_callback_is_not_reported_dropped() {
        let manager = ScreenshotManager::new();
        manager.insert_screenshot(4, ScreenshotState::pending(ScreenshotKind::Viewport));
        let callback = manager.callback(4, egui::ViewportId::ROOT, true);
        let clone = callback.clone();
        callback.complete(white_pixel());
        drop(clone);

        let snapshot = manager.screenshot_debug_snapshot(true, 0);
        assert_eq!(snapshot.debug.images_received, 1);
        assert_eq!(snapshot.debug.callbacks_dropped, 0);
    }

    #[test]
    fn sent_commands_record_their_frame_in_queue_order() {
        let manager = ScreenshotManager::new();
        let viewport_id = egui::ViewportId::ROOT;
        for request_id in [1, 2] {
            manager.insert_screenshot(
                request_id,
                ScreenshotState::pending(ScreenshotKind::Viewport),
            );
        }
        let _first = manager.callback(1, viewport_id, false);
        let _second = manager.callback(2, viewport_id, false);

        assert_eq!(
            manager.record_screenshot_command_sent(viewport_id, false, 10),
            Some(1)
        );
        assert_eq!(
            manager.record_screenshot_command_sent(viewport_id, false, 11),
            Some(2)
        );
        assert_eq!(
            manager.record_screenshot_command_sent(viewport_id, false, 12),
            None
        );
        assert_eq!(
            manager.screenshot_state(1).expect("state").sent_frame(),
            Some(10)
        );
        assert_eq!(
            manager.screenshot_state(2).expect("state").sent_frame(),
            Some(11)
        );
    }

    #[test]
    fn screenshot_debug_snapshot_reports_verbose_flag() {
        let manager = ScreenshotManager::new();
        let snapshot = manager.screenshot_debug_snapshot(false, 3);
        assert!(!snapshot.enabled);
        assert_eq!(snapshot.frame_count, 3);
    }
}
