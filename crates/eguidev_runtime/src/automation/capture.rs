//! Capture metadata shared by screenshot and sampling operations.

use std::ops::Range;

use image::codecs::png::PngEncoder;

use super::*;

/// Encoding that a screenshot request selects.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ScreenshotFormat {
    /// Lossy encoding. It keeps a capture small.
    #[default]
    Jpeg,
    /// Lossless encoding. It keeps every rendered pixel exact.
    Png,
}

impl ScreenshotFormat {
    /// Return the MCP media type of this encoding.
    pub(super) fn media_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    /// Parse one script-supplied format word.
    pub(super) fn parse(word: &str) -> Option<Self> {
        match word {
            "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

/// Encoding options for one screenshot.
///
/// The default keeps the established JPEG encoding and the 1,600 pixel long
/// edge, so a call that passes no options is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScreenshotOptions {
    /// Selected encoding.
    pub(super) format: ScreenshotFormat,
    /// Largest long edge in pixels. Zero keeps the captured size.
    pub(super) max_dimension: u32,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            format: ScreenshotFormat::default(),
            max_dimension: DEFAULT_MAX_SCREENSHOT_DIMENSION,
        }
    }
}

/// One encoded capture and the media type that describes it.
pub(super) struct EncodedImage {
    /// Base64 image payload.
    pub(super) data: String,
    /// MCP media type, such as `image/png`.
    pub(super) media_type: &'static str,
}

pub(super) fn capture_pixels_per_point(inner: &Inner, viewport_id: egui::ViewportId) -> f32 {
    inner
        .viewports
        .input_snapshot(viewport_id)
        .map(|snapshot| snapshot.pixels_per_point)
        .unwrap_or(1.0)
}

pub(super) fn captured_viewport_name(
    inner: &Inner,
    viewport_id: egui::ViewportId,
) -> Option<String> {
    viewport_snapshot_for(inner, viewport_id).and_then(|viewport| viewport.name)
}

impl DevMcpServer {
    /// Capture a viewport once and sample exact RGBA pixels at logical
    /// positions.
    pub(super) async fn viewport_sample_pixels(
        &self,
        viewport_id: Option<String>,
        positions: Vec<Pos2>,
    ) -> ToolResult<Vec<PixelSample>> {
        let viewport_id = resolve_viewport_id(&self.inner, viewport_id)?;
        let pixels_per_point = capture_pixels_per_point(&self.inner, viewport_id);
        let image = capture_screenshot_image(&self.inner, &self.runtime, viewport_id).await?;
        sample_color_image(&image, pixels_per_point, &positions).map_err(Into::into)
    }

    /// Capture a widget's viewport once and sample relative widget positions
    /// from it.
    pub(super) async fn widget_sample_pixels(
        &self,
        viewport_id: Option<String>,
        target: &WidgetRef,
        positions: Vec<Pos2>,
    ) -> ToolResult<Vec<PixelSample>> {
        let (widget, viewport_id) =
            resolve_widget_and_viewport(&self.inner, viewport_id.as_deref(), target)?;
        let pixels_per_point = capture_pixels_per_point(&self.inner, viewport_id);
        let viewport_name = captured_viewport_name(&self.inner, viewport_id);
        let image = capture_screenshot_image(&self.inner, &self.runtime, viewport_id).await?;
        sample_widget_relative_pixels(
            &image,
            pixels_per_point,
            &widget,
            viewport_name.as_deref(),
            &positions,
        )
        .map_err(Into::into)
    }

    /// Capture a widget's viewport once and sample a grid over its visible
    /// rect.
    pub(super) async fn widget_sample_grid(
        &self,
        viewport_id: Option<String>,
        target: &WidgetRef,
        nx: usize,
        ny: usize,
    ) -> ToolResult<Vec<PixelSample>> {
        let (widget, viewport_id) =
            resolve_widget_and_viewport(&self.inner, viewport_id.as_deref(), target)?;
        let pixels_per_point = capture_pixels_per_point(&self.inner, viewport_id);
        let viewport_name = captured_viewport_name(&self.inner, viewport_id);
        let image = capture_screenshot_image(&self.inner, &self.runtime, viewport_id).await?;
        sample_widget_grid(
            &image,
            pixels_per_point,
            &widget,
            viewport_name.as_deref(),
            nx,
            ny,
        )
        .map_err(Into::into)
    }
}

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_WAIT_TIMEOUT: Duration = Duration::from_millis(500);

enum ScreenshotWaitOutcome {
    Ready,
    NativeCapture(ScreenshotState),
}

/// Release retained screenshot state even when the enclosing script is
/// cancelled.
struct ScreenshotRequest<'a> {
    /// Runtime that retains this request until it completes.
    runtime: &'a Runtime,
    /// Unique request owned by the capture future.
    request_id: u64,
}

impl Drop for ScreenshotRequest<'_> {
    fn drop(&mut self) {
        self.runtime.take_screenshot(self.request_id);
    }
}

pub(super) fn resolve_screenshot_viewport(
    inner: &Inner,
    viewport_id: Option<String>,
) -> Result<egui::ViewportId, ToolError> {
    if let Some(viewport_id) = viewport_id {
        return resolve_viewport_id(inner, Some(viewport_id));
    }
    Ok(egui::ViewportId::ROOT)
}

pub(super) async fn capture_screenshot(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
    kind: ScreenshotKind,
    options: ScreenshotOptions,
) -> Result<EncodedImage, ToolError> {
    let state = capture_screenshot_state(inner, runtime, viewport_id, kind).await?;
    build_screenshot_data(&state, options)
}

/// Capture the complete native window, including platform chrome.
#[cfg(target_os = "macos")]
pub(super) fn capture_native_screenshot(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    options: ScreenshotOptions,
) -> Result<EncodedImage, ToolError> {
    let snapshot = viewport_snapshot_for(inner, viewport_id).ok_or_else(|| {
        ToolError::new(
            ErrorCode::NotActionable,
            "Viewport not ready for a native screenshot",
        )
    })?;
    let title = snapshot.title.as_deref().ok_or_else(|| {
        ToolError::new(
            ErrorCode::NotActionable,
            "Viewport has no title to match a native window",
        )
    })?;
    let window_number = window_number_for_title(title).map_err(|error| {
        ToolError::new(
            ErrorCode::NotActionable,
            format!("Could not resolve native window: {error}"),
        )
    })?;
    let image = capture_window_image(window_number).map_err(|error| {
        ToolError::new(
            ErrorCode::NotActionable,
            format!("Could not capture native window: {error}"),
        )
    })?;
    encode_screenshot(&image, options)
}

/// Report that native screenshots are not available on this platform.
#[cfg(not(target_os = "macos"))]
pub(super) fn capture_native_screenshot(
    _inner: &Inner,
    _viewport_id: egui::ViewportId,
    _options: ScreenshotOptions,
) -> Result<EncodedImage, ToolError> {
    Err(ToolError::new(
        ErrorCode::Unsupported,
        "Native window screenshots are only available on macOS",
    ))
}

async fn capture_screenshot_image(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
) -> Result<Arc<egui::ColorImage>, ToolError> {
    let state =
        capture_screenshot_state(inner, runtime, viewport_id, ScreenshotKind::Viewport).await?;
    build_screenshot_image(&state)
}

async fn capture_screenshot_state(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
    kind: ScreenshotKind,
) -> Result<ScreenshotState, ToolError> {
    // Best-effort wake-up before sending the screenshot command. Some idle
    // windows won't produce a frame until a command is queued, so only
    // treat this as fatal if context capture is not ready yet.
    let event_loop_ready = ensure_event_loop_active(inner, runtime, viewport_id).await;
    let has_snapshot = inner.viewports.has_viewport_snapshot(viewport_id);
    if !inner.has_context() {
        if let Err(error) = event_loop_ready {
            return Err(error);
        }
        return Err(ToolError::new(
            ErrorCode::NotActionable,
            "Viewport context not ready for screenshots",
        )
        .with_details(screenshot_error_details(inner, runtime, viewport_id)));
    }
    if !has_snapshot {
        event_loop_ready?;
        return Err(ToolError::new(
            ErrorCode::NotActionable,
            "Viewport not ready for screenshots",
        )
        .with_details(screenshot_error_details(inner, runtime, viewport_id)));
    }

    let start_frame = viewport_frame(inner, viewport_id);
    let request_id = inner.next_request_id();
    let kind_snapshot = kind.clone();
    runtime.insert_screenshot(request_id, ScreenshotState::pending(kind));
    inner.queue_command(
        viewport_id,
        egui::ViewportCommand::Screenshot(runtime.screenshot_callback(
            inner,
            request_id,
            viewport_id,
        )),
    );
    runtime.record_screenshot_request(inner, request_id, viewport_id, &kind_snapshot);
    await_screenshot(
        inner,
        runtime,
        request_id,
        viewport_id,
        &kind_snapshot,
        start_frame,
    )
    .await
}

/// The frame count of the last frame of `viewport_id`, or zero before its
/// first frame.
pub(super) fn viewport_frame(inner: &Inner, viewport_id: egui::ViewportId) -> u64 {
    inner
        .viewports
        .capture_snapshot(viewport_id)
        .map(|snapshot| snapshot.frame_count)
        .unwrap_or(0)
}

async fn ensure_event_loop_active(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
) -> Result<(), ToolError> {
    let initial_frame = viewport_frame(inner, viewport_id);

    // Wait for at least one frame to process. Use a short poll interval with
    // periodic repaint requests so we recover when the event loop stalls.
    let frame_wait = async {
        loop {
            let current_frame = viewport_frame(inner, viewport_id);
            if current_frame > initial_frame {
                return;
            }
            let mut notified = std::pin::pin!(runtime.frame_notify().notified());
            notified.as_mut().enable();
            inner.request_repaint_of(viewport_id);
            let poll = Duration::from_millis(DEFAULT_POLL_INTERVAL_MS);
            drop(timeout(poll, notified.as_mut()).await);
        }
    };

    if timeout(FRAME_WAIT_TIMEOUT, frame_wait).await.is_err() {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "Window event loop not responding. The window may be minimized or hidden. \
For eframe apps, prefer Renderer::Glow for automation; Wgpu backends can stall idle frames.",
        )
        .with_details(screenshot_error_details(inner, runtime, viewport_id)));
    }

    Ok(())
}

/// Wait for the callback of request `request_id`.
///
/// The command goes to egui once, and egui answers the callback when it paints
/// the next frame of the viewport. A deferred viewport always answers. An
/// immediate child viewport never does, since eframe paints it inside its
/// parent and does not read its pixels, so on macOS a child viewport that
/// renders [`CALLBACK_FRAMES`] frames after the command without an answer falls
/// back to a native window capture.
async fn await_screenshot(
    inner: &Inner,
    runtime: &Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
    start_frame: u64,
) -> Result<ScreenshotState, ToolError> {
    let request = ScreenshotRequest {
        runtime,
        request_id,
    };
    let notify = match runtime.screenshot_state(request_id) {
        Some(state) => state.notify(),
        None => {
            return Err(
                ToolError::new(ErrorCode::NotFound, "Unknown request id").with_details(
                    screenshot_request_details(inner, runtime, request_id, viewport_id, kind),
                ),
            );
        }
    };

    let wait_loop = async {
        let mut last_frame = start_frame;
        let mut frames_after_send = 0_u32;
        loop {
            // Arm both wake-ups before reading the state, so an answer or a
            // frame that lands between the read and the wait is not lost.
            let mut answered = std::pin::pin!(notify.notified());
            answered.as_mut().enable();
            let mut frame = std::pin::pin!(runtime.frame_notify().notified());
            frame.as_mut().enable();

            let Some(state) = runtime.screenshot_state(request_id) else {
                return Err(ToolError::new(ErrorCode::NotFound, "Unknown request id")
                    .with_details(screenshot_request_details(
                        inner,
                        runtime,
                        request_id,
                        viewport_id,
                        kind,
                    )));
            };
            if state.is_ready() {
                return Ok(ScreenshotWaitOutcome::Ready);
            }
            if state.is_dropped() {
                return Err(ToolError::new(
                    ErrorCode::NotActionable,
                    "egui dropped the screenshot request before the viewport painted. \
                     The viewport may have closed.",
                )
                .with_details(screenshot_request_details(
                    inner,
                    runtime,
                    request_id,
                    viewport_id,
                    kind,
                )));
            }

            let current_frame = viewport_frame(inner, viewport_id);
            if current_frame > last_frame {
                last_frame = current_frame;
                if state.sent_frame().is_some_and(|sent| current_frame > sent) {
                    frames_after_send = frames_after_send.saturating_add(1);
                }
            }
            if should_try_native_screenshot_fallback(viewport_id, frames_after_send) {
                // A native capture can disagree with the recorded viewport
                // while a resize settles, so retry it against each fresh frame
                // until the screenshot deadline.
                match native_screenshot_fallback(inner, viewport_id, kind) {
                    Ok(state) => return Ok(ScreenshotWaitOutcome::NativeCapture(state)),
                    Err(error) => runtime.log_screenshot(
                        inner,
                        format!(
                            "native fallback retry request_id={request_id} viewport={} error={error}",
                            viewport_id_to_string(viewport_id),
                        ),
                    ),
                }
            }
            // egui schedules the frame that answers the command. Each further
            // request covers a viewport that renders only when asked, such as
            // an immediate child that waits for the native fallback.
            inner.request_repaint_of(viewport_id);
            tokio::select! {
                () = answered => {}
                () = frame => {}
            }
        }
    };

    let outcome =
        match timeout(SCREENSHOT_TIMEOUT, wait_loop).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                drop(request);
                let end_frame = inner.frame_count();
                runtime.log_screenshot(
                    inner,
                    format!(
                        "timeout request_id={request_id} viewport={} start_frame={start_frame} \
                     end_frame={end_frame}",
                        viewport_id_to_string(viewport_id),
                    ),
                );
                match native_screenshot_fallback(inner, viewport_id, kind) {
                    Ok(state) => ScreenshotWaitOutcome::NativeCapture(state),
                    Err(fallback_error) => {
                        return Err(ToolError::new(
                            ErrorCode::Timeout,
                            screenshot_timeout_message(viewport_id, &fallback_error),
                        )
                        .with_details(screenshot_timeout_details(&ScreenshotTimeoutContext {
                            inner,
                            runtime,
                            request_id,
                            viewport_id,
                            kind,
                            start_frame,
                            end_frame,
                            fallback_error: &fallback_error,
                        })));
                    }
                }
            }
        };

    match outcome {
        ScreenshotWaitOutcome::Ready => runtime.take_screenshot(request_id).ok_or_else(|| {
            ToolError::new(ErrorCode::NotFound, "Unknown request id").with_details(
                screenshot_request_details_with_frames(
                    inner,
                    runtime,
                    request_id,
                    viewport_id,
                    kind,
                    start_frame,
                    inner.frame_count(),
                ),
            )
        }),
        ScreenshotWaitOutcome::NativeCapture(state) => {
            runtime.log_screenshot(
                inner,
                format!(
                    "native fallback succeeded request_id={request_id} viewport={}",
                    viewport_id_to_string(viewport_id),
                ),
            );
            Ok(state)
        }
    }
}

/// The frames that a viewport renders after the frame that sent a screenshot
/// command, by which egui has answered the callback if it ever will. The next
/// frame paints and answers it, and the frame after that observes the answer.
const CALLBACK_FRAMES: u32 = 2;

/// Whether a child viewport has rendered enough frames after its command,
/// without an answer, to justify a native window capture.
pub(super) fn should_try_native_screenshot_fallback(
    viewport_id: egui::ViewportId,
    frames_after_send: u32,
) -> bool {
    native_fallback_applies(viewport_id) && frames_after_send >= CALLBACK_FRAMES
}

fn build_screenshot_data(
    state: &ScreenshotState,
    options: ScreenshotOptions,
) -> Result<EncodedImage, ToolError> {
    let image = build_screenshot_image(state)?;
    encode_screenshot(&image, options)
}

fn build_screenshot_image(state: &ScreenshotState) -> Result<Arc<egui::ColorImage>, ToolError> {
    let Some(image) = state.image() else {
        return Err(ToolError::new(
            ErrorCode::Internal,
            "Screenshot missing image",
        ));
    };
    let image = match &state.kind {
        ScreenshotKind::Viewport => image,
        ScreenshotKind::Widget {
            rect,
            pixels_per_point,
        } => crop_image(&image, *rect, *pixels_per_point)?,
    };
    Ok(image)
}

#[cfg(target_os = "macos")]
fn native_screenshot_fallback(
    inner: &Inner,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
) -> Result<ScreenshotState, String> {
    if viewport_id == egui::ViewportId::ROOT {
        return Err("native fallback is only used for child viewports".to_string());
    }
    let snapshot = viewport_snapshot_for(inner, viewport_id)
        .ok_or_else(|| "viewport snapshot was unavailable".to_string())?;
    let title = snapshot
        .title
        .as_deref()
        .ok_or_else(|| "viewport has no title to match a native window".to_string())?;
    let window_number = window_number_for_title(title)?;
    let mut state = ScreenshotState::pending(kind.clone());
    let image = crop_native_capture_to_viewport(capture_window_image(window_number)?, &snapshot)?;
    state.mark_ready(Arc::new(image));
    Ok(state)
}

#[cfg(not(target_os = "macos"))]
fn native_screenshot_fallback(
    _inner: &Inner,
    _viewport_id: egui::ViewportId,
    _kind: &ScreenshotKind,
) -> Result<ScreenshotState, String> {
    Err("native fallback is only available on macOS".to_string())
}

fn screenshot_error_details(
    inner: &Inner,
    runtime: &Runtime,
    viewport_id: egui::ViewportId,
) -> Value {
    let snapshots = inner.viewports.viewports_snapshot();
    let known_viewports = snapshots
        .iter()
        .map(|snapshot| snapshot.viewport_id.clone())
        .collect::<Vec<_>>();
    serde_json::json!({
        "viewport_id": viewport_id_to_string(viewport_id),
        "has_context": inner.has_context(),
        "known_viewports": known_viewports,
        "frame_count": inner.frame_count(),
        "has_snapshot": inner.viewports.has_viewport_snapshot(viewport_id),
        "debug": runtime.screenshot_debug_snapshot(inner),
    })
}

fn screenshot_request_details(
    inner: &Inner,
    runtime: &Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
) -> Value {
    screenshot_request_details_with_frames(inner, runtime, request_id, viewport_id, kind, 0, 0)
}

struct ScreenshotTimeoutContext<'a> {
    inner: &'a Inner,
    runtime: &'a Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &'a ScreenshotKind,
    start_frame: u64,
    end_frame: u64,
    fallback_error: &'a str,
}

fn screenshot_timeout_details(context: &ScreenshotTimeoutContext<'_>) -> Value {
    let mut details = screenshot_request_details_with_frames(
        context.inner,
        context.runtime,
        context.request_id,
        context.viewport_id,
        context.kind,
        context.start_frame,
        context.end_frame,
    );
    if let Some(map) = details.as_object_mut() {
        map.insert(
            "native_fallback".to_string(),
            json!({
                "attempted": native_fallback_applies(context.viewport_id),
                "error": context.fallback_error,
            }),
        );
    }
    details
}

pub(super) fn screenshot_timeout_message(
    viewport_id: egui::ViewportId,
    fallback_error: &str,
) -> String {
    if native_fallback_applies(viewport_id) {
        return format!(
            "Screenshot deadline exceeded for this child viewport. egui did not answer the \
             screenshot callback, eguidev retried the macOS native window capture against fresh \
             frames until the deadline, and the last attempt failed: {fallback_error}."
        );
    }
    format!(
        "Screenshot timed out waiting for egui to answer the screenshot callback. The viewport \
         did not paint a frame after the command. Native screenshot fallback is only available \
         for child viewports on macOS: {fallback_error}."
    )
}

fn native_fallback_applies(viewport_id: egui::ViewportId) -> bool {
    cfg!(target_os = "macos") && viewport_id != egui::ViewportId::ROOT
}

pub(super) fn crop_native_capture_to_viewport(
    image: egui::ColorImage,
    snapshot: &ViewportSnapshot,
) -> Result<egui::ColorImage, String> {
    let target_width = scaled_viewport_pixels(snapshot.inner_size.x, snapshot.pixels_per_point)?;
    let target_height = scaled_viewport_pixels(snapshot.inner_size.y, snapshot.pixels_per_point)?;
    if target_width == 0 || target_height == 0 {
        return Err("viewport content size is empty".to_string());
    }
    if image.size == [target_width, target_height] {
        return Ok(image);
    }
    if image.size[0] < target_width || image.size[1] < target_height {
        return Err(format!(
            "native capture {}x{} is smaller than viewport content {}x{}",
            image.size[0], image.size[1], target_width, target_height
        ));
    }

    let x0 = (image.size[0] - target_width) / 2;
    let y0 = image.size[1] - target_height;
    let mut pixels = Vec::with_capacity(target_width * target_height);
    for y in y0..(y0 + target_height) {
        let row_start = y * image.size[0] + x0;
        pixels.extend_from_slice(&image.pixels[row_start..row_start + target_width]);
    }
    Ok(egui::ColorImage {
        size: [target_width, target_height],
        source_size: egui::Vec2::new(target_width as f32, target_height as f32),
        pixels,
    })
}

fn scaled_viewport_pixels(size: f32, pixels_per_point: f32) -> Result<usize, String> {
    let pixels = size * pixels_per_point;
    if !pixels.is_finite() || pixels < 0.0 {
        return Err("viewport content size is not finite".to_string());
    }
    Ok(pixels.round() as usize)
}

fn screenshot_request_details_with_frames(
    inner: &Inner,
    runtime: &Runtime,
    request_id: u64,
    viewport_id: egui::ViewportId,
    kind: &ScreenshotKind,
    start_frame: u64,
    end_frame: u64,
) -> Value {
    let kind_details = match kind {
        ScreenshotKind::Viewport => serde_json::json!({ "kind": "viewport" }),
        ScreenshotKind::Widget {
            rect,
            pixels_per_point,
        } => serde_json::json!({
            "kind": "widget",
            "rect": rect,
            "pixels_per_point": pixels_per_point,
        }),
    };
    serde_json::json!({
        "request_id": request_id,
        "viewport_id": viewport_id_to_string(viewport_id),
        "kind": kind_details,
        "start_frame": start_frame,
        "end_frame": end_frame,
        "debug": runtime.screenshot_debug_snapshot(inner),
    })
}

const DEFAULT_MAX_SCREENSHOT_DIMENSION: u32 = 1600;

/// Reduce an image so its long edge fits `max_dimension`.
///
/// The reduction averages every source pixel that a destination pixel covers.
/// A point sample would alias rendered text, and a screenshot reduced to a
/// thumbnail must stay readable.
fn scale_screenshot_image(image: &egui::ColorImage, max_dimension: u32) -> egui::ColorImage {
    let width = image.width();
    let height = image.height();
    let long_edge = width.max(height) as u32;
    if long_edge <= max_dimension || max_dimension == 0 {
        return image.clone();
    }
    let scale = f64::from(max_dimension) / f64::from(long_edge);
    let new_width = (((width as f64) * scale).round() as usize).max(1);
    let new_height = (((height as f64) * scale).round() as usize).max(1);
    let mut pixels = Vec::with_capacity(new_width * new_height);
    for y in 0..new_height {
        let y0 = y * height / new_height;
        let y1 = (((y + 1) * height).div_ceil(new_height))
            .min(height)
            .max(y0 + 1);
        for x in 0..new_width {
            let x0 = x * width / new_width;
            let x1 = (((x + 1) * width).div_ceil(new_width))
                .min(width)
                .max(x0 + 1);
            pixels.push(average_pixel(image, x0..x1, y0..y1));
        }
    }
    egui::ColorImage {
        size: [new_width, new_height],
        source_size: egui::Vec2::new(new_width as f32, new_height as f32),
        pixels,
    }
}

/// Average one source rectangle into a single destination pixel.
fn average_pixel(image: &egui::ColorImage, xs: Range<usize>, ys: Range<usize>) -> egui::Color32 {
    let mut totals = [0u64; 4];
    let mut count = 0u64;
    for y in ys {
        for x in xs.clone() {
            let Some(pixel) = image.pixels.get(y * image.width() + x) else {
                continue;
            };
            let [r, g, b, a] = pixel.to_array();
            totals[0] += u64::from(r);
            totals[1] += u64::from(g);
            totals[2] += u64::from(b);
            totals[3] += u64::from(a);
            count += 1;
        }
    }
    if count == 0 {
        return egui::Color32::TRANSPARENT;
    }
    egui::Color32::from_rgba_premultiplied(
        (totals[0] / count) as u8,
        (totals[1] / count) as u8,
        (totals[2] / count) as u8,
        (totals[3] / count) as u8,
    )
}

/// Encode a captured image under the requested options.
fn encode_screenshot(
    image: &egui::ColorImage,
    options: ScreenshotOptions,
) -> Result<EncodedImage, ToolError> {
    let image = scale_screenshot_image(image, options.max_dimension);
    match options.format {
        ScreenshotFormat::Jpeg => encode_jpeg(&image),
        ScreenshotFormat::Png => encode_png(&image),
    }
}

/// Encode one image as PNG, which keeps every rendered pixel exact.
fn encode_png(image: &egui::ColorImage) -> Result<EncodedImage, ToolError> {
    let width = image.size[0] as u32;
    let height = image.size[1] as u32;
    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    let mut png_data = Vec::new();
    let encoder = PngEncoder::new(&mut png_data);
    image::ImageEncoder::write_image(
        encoder,
        &bytes,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| ToolError::new(ErrorCode::Internal, format!("PNG encode failed: {error}")))?;
    Ok(EncodedImage {
        data: STANDARD.encode(png_data),
        media_type: ScreenshotFormat::Png.media_type(),
    })
}

fn encode_jpeg(image: &egui::ColorImage) -> Result<EncodedImage, ToolError> {
    const JPEG_QUALITY: u8 = 88;
    let width = image.size[0] as u32;
    let height = image.size[1] as u32;
    let capacity = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .unwrap_or(0) as usize;
    let mut bytes = Vec::with_capacity(capacity);
    for pixel in &image.pixels {
        let [r, g, b, a] = pixel.to_array();
        // Color32 channels already include alpha in gamma space. Add the
        // white matte once, retaining the captured color convention.
        let white = 255 - a;
        bytes.extend_from_slice(&[
            r.saturating_add(white),
            g.saturating_add(white),
            b.saturating_add(white),
        ]);
    }
    let mut jpeg_data = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut jpeg_data, JPEG_QUALITY);
    image::ImageEncoder::write_image(
        encoder,
        &bytes,
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|error| ToolError::new(ErrorCode::Internal, format!("JPEG encode failed: {error}")))?;
    Ok(EncodedImage {
        data: STANDARD.encode(jpeg_data),
        media_type: ScreenshotFormat::Jpeg.media_type(),
    })
}

fn crop_image(
    image: &egui::ColorImage,
    rect: Rect,
    pixels_per_point: f32,
) -> Result<Arc<egui::ColorImage>, ToolError> {
    let width = image.size[0] as i32;
    let height = image.size[1] as i32;
    let min_x = (rect.min.x * pixels_per_point).round() as i32;
    let min_y = (rect.min.y * pixels_per_point).round() as i32;
    let max_x = (rect.max.x * pixels_per_point).round() as i32;
    let max_y = (rect.max.y * pixels_per_point).round() as i32;
    let x0 = min_x.clamp(0, width);
    let y0 = min_y.clamp(0, height);
    let x1 = max_x.clamp(0, width);
    let y1 = max_y.clamp(0, height);
    let crop_width = (x1 - x0).max(0) as usize;
    let crop_height = (y1 - y0).max(0) as usize;
    if crop_width == 0 || crop_height == 0 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgument,
            "Widget rect is empty",
        ));
    }
    let mut pixels = Vec::with_capacity(crop_width * crop_height);
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = (y as usize) * image.size[0] + x as usize;
            if let Some(pixel) = image.pixels.get(idx) {
                pixels.push(*pixel);
            }
        }
    }
    Ok(Arc::new(egui::ColorImage {
        size: [crop_width, crop_height],
        source_size: egui::Vec2::new(crop_width as f32, crop_height as f32),
        pixels,
    }))
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PixelSample {
    pub(super) position: Pos2,
    pub(super) physical: [usize; 2],
    pub(super) rgba: [u8; 4],
    pub(super) hex: String,
}

pub(super) fn sample_color_image(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    positions: &[Pos2],
) -> Result<Vec<PixelSample>, ToolError> {
    ensure_valid_pixels_per_point(pixels_per_point)?;
    positions
        .iter()
        .map(|position| sample_color_pixel(image, pixels_per_point, *position))
        .collect()
}

pub(super) fn sample_color_pixel(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    position: Pos2,
) -> Result<PixelSample, ToolError> {
    let physical_x = (position.x * pixels_per_point).floor();
    let physical_y = (position.y * pixels_per_point).floor();
    if !physical_x.is_finite() || !physical_y.is_finite() {
        return Err(ToolError::new(
            ErrorCode::InvalidArgument,
            "Sample position must be finite",
        ));
    }
    if physical_x < 0.0
        || physical_y < 0.0
        || physical_x >= image.size[0] as f32
        || physical_y >= image.size[1] as f32
    {
        return Err(ToolError::new(
            ErrorCode::InvalidArgument,
            "Sample position is outside the captured image",
        )
        .with_details(json!({
            "position": position,
            "physical": [physical_x, physical_y],
            "image_size": image.size,
            "pixels_per_point": pixels_per_point,
        })));
    }
    Ok(pixel_sample(
        image,
        position,
        physical_x as usize,
        physical_y as usize,
    ))
}

/// The sample of the image pixel at physical `x` and `y`, reported at
/// `position`. The caller has checked that the pixel is inside the image.
fn pixel_sample(image: &egui::ColorImage, position: Pos2, x: usize, y: usize) -> PixelSample {
    let rgba = image.pixels[y * image.size[0] + x].to_array();
    PixelSample {
        position,
        physical: [x, y],
        rgba,
        hex: format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            rgba[0], rgba[1], rgba[2], rgba[3]
        ),
    }
}

pub(super) fn sample_widget_relative_pixels(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    widget: &WidgetRegistryEntry,
    viewport_name: Option<&str>,
    positions: &[Pos2],
) -> Result<Vec<PixelSample>, ToolError> {
    ensure_valid_pixels_per_point(pixels_per_point)?;
    let visible_rect = widget_visible_rect(widget, image, pixels_per_point);
    positions
        .iter()
        .map(|position| {
            let absolute = Pos2 {
                x: widget.rect.min.x + position.x,
                y: widget.rect.min.y + position.y,
            };
            if !position.x.is_finite()
                || !position.y.is_finite()
                || !point_in_rect_exclusive(absolute, widget.rect)
            {
                return Err(widget_sample_error(
                    ErrorCode::InvalidArgument,
                    "Sample position is outside the widget rect",
                    widget,
                    visible_rect,
                    viewport_name,
                    pixels_per_point,
                    json!({
                        "position": position,
                        "absolute_position": absolute,
                    }),
                ));
            }
            sample_widget_absolute_pixel(
                image,
                pixels_per_point,
                widget,
                visible_rect,
                viewport_name,
                *position,
                absolute,
            )
        })
        .collect()
}

pub(super) fn sample_widget_grid(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    widget: &WidgetRegistryEntry,
    viewport_name: Option<&str>,
    nx: usize,
    ny: usize,
) -> Result<Vec<PixelSample>, ToolError> {
    ensure_valid_pixels_per_point(pixels_per_point)?;
    let visible_rect = widget_visible_rect(widget, image, pixels_per_point).ok_or_else(|| {
        widget_sample_error(
            ErrorCode::NotActionable,
            "Widget is not visible enough to sample",
            widget,
            None,
            viewport_name,
            pixels_per_point,
            json!({}),
        )
    })?;
    let sample_rect = inset_sampling_rect(visible_rect).ok_or_else(|| {
        widget_sample_error(
            ErrorCode::NotActionable,
            "Widget visible area is too small to sample",
            widget,
            Some(visible_rect),
            viewport_name,
            pixels_per_point,
            json!({}),
        )
    })?;
    let capacity = nx.checked_mul(ny).ok_or_else(|| {
        ToolError::new(ErrorCode::InvalidArgument, "sample_grid nx * ny overflowed")
    })?;
    let mut samples = Vec::with_capacity(capacity);
    for y in grid_axis(sample_rect.min.y, sample_rect.max.y, ny) {
        for x in grid_axis(sample_rect.min.x, sample_rect.max.x, nx) {
            let absolute = Pos2 { x, y };
            let relative = Pos2 {
                x: absolute.x - widget.rect.min.x,
                y: absolute.y - widget.rect.min.y,
            };
            samples.push(sample_widget_absolute_pixel(
                image,
                pixels_per_point,
                widget,
                Some(visible_rect),
                viewport_name,
                relative,
                absolute,
            )?);
        }
    }
    Ok(samples)
}

pub(super) fn sample_widget_absolute_pixel(
    image: &egui::ColorImage,
    pixels_per_point: f32,
    widget: &WidgetRegistryEntry,
    visible_rect: Option<Rect>,
    viewport_name: Option<&str>,
    relative_position: Pos2,
    absolute_position: Pos2,
) -> Result<PixelSample, ToolError> {
    let physical_x = (absolute_position.x * pixels_per_point).floor();
    let physical_y = (absolute_position.y * pixels_per_point).floor();
    if !physical_x.is_finite()
        || !physical_y.is_finite()
        || physical_x < 0.0
        || physical_y < 0.0
        || physical_x >= image.size[0] as f32
        || physical_y >= image.size[1] as f32
    {
        return Err(widget_sample_error(
            ErrorCode::InvalidArgument,
            "Sample position is outside the captured image",
            widget,
            visible_rect,
            viewport_name,
            pixels_per_point,
            json!({
                "position": relative_position,
                "absolute_position": absolute_position,
                "physical": [physical_x, physical_y],
                "image_size": image.size,
            }),
        ));
    }
    Ok(pixel_sample(
        image,
        relative_position,
        physical_x as usize,
        physical_y as usize,
    ))
}

pub(super) fn ensure_valid_pixels_per_point(pixels_per_point: f32) -> Result<(), ToolError> {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        return Ok(());
    }
    Err(ToolError::new(
        ErrorCode::Internal,
        "Invalid pixels_per_point for pixel sampling",
    ))
}

pub(super) fn widget_visible_rect(
    widget: &WidgetRegistryEntry,
    image: &egui::ColorImage,
    pixels_per_point: f32,
) -> Option<Rect> {
    if !widget.visible {
        return None;
    }
    let mut rect = widget.rect;
    if let Some(layout) = &widget.layout {
        rect = rect_intersection(rect, layout.clip_rect)?;
    }
    rect_intersection(rect, image_logical_rect(image, pixels_per_point))
}

pub(super) fn image_logical_rect(image: &egui::ColorImage, pixels_per_point: f32) -> Rect {
    Rect {
        min: Pos2 { x: 0.0, y: 0.0 },
        max: Pos2 {
            x: image.size[0] as f32 / pixels_per_point,
            y: image.size[1] as f32 / pixels_per_point,
        },
    }
}

pub(super) fn point_in_rect_exclusive(position: Pos2, rect: Rect) -> bool {
    position.x >= rect.min.x
        && position.y >= rect.min.y
        && position.x < rect.max.x
        && position.y < rect.max.y
}

pub(super) fn inset_sampling_rect(rect: Rect) -> Option<Rect> {
    let width = rect.max.x - rect.min.x;
    let height = rect.max.y - rect.min.y;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let inset_x = WIDGET_SAMPLE_GRID_INSET.min(width * 0.25);
    let inset_y = WIDGET_SAMPLE_GRID_INSET.min(height * 0.25);
    let rect = Rect {
        min: Pos2 {
            x: rect.min.x + inset_x,
            y: rect.min.y + inset_y,
        },
        max: Pos2 {
            x: rect.max.x - inset_x,
            y: rect.max.y - inset_y,
        },
    };
    rect_is_positive(rect).then_some(rect)
}

pub(super) fn rect_is_positive(rect: Rect) -> bool {
    (rect.max.x - rect.min.x) > 0.0 && (rect.max.y - rect.min.y) > 0.0
}

pub(super) fn grid_axis(min: f32, max: f32, count: usize) -> Vec<f32> {
    if count == 1 {
        return vec![(min + max) * 0.5];
    }
    let step = (max - min) / (count - 1) as f32;
    (0..count).map(|index| min + step * index as f32).collect()
}

pub(super) fn widget_sample_error(
    code: ErrorCode,
    message: &'static str,
    widget: &WidgetRegistryEntry,
    visible_rect: Option<Rect>,
    viewport_name: Option<&str>,
    pixels_per_point: f32,
    extra: Value,
) -> ToolError {
    let mut details = json!({
        "widget_id": widget.id,
        "viewport_id": widget.viewport_id,
        "viewport_name": viewport_name,
        "rect": widget.rect,
        "visible_rect": visible_rect,
        "pixels_per_point": pixels_per_point,
    });
    if let (Some(details), Value::Object(extra)) = (details.as_object_mut(), extra) {
        for (key, value) in extra {
            details.insert(key, value);
        }
    }
    ToolError::new(code, message).with_details(details)
}

#[cfg(test)]
mod tests {
    use std::{future::poll_fn, sync::Arc, task::Poll};

    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::{
        DEFAULT_MAX_SCREENSHOT_DIMENSION, ScreenshotFormat, ScreenshotOptions, await_screenshot,
        encode_screenshot, scale_screenshot_image,
    };
    use crate::{
        registry::Inner,
        runtime::Runtime,
        screenshots::{ScreenshotKind, ScreenshotState},
    };

    #[tokio::test]
    async fn cancelled_screenshot_releases_only_its_request() {
        for arrives_before_cancellation in [false, true] {
            let inner = Arc::new(Inner::new());
            let runtime = Runtime::ensure_for_inner(&inner);
            runtime.insert_screenshot(1, ScreenshotState::pending(ScreenshotKind::Viewport));
            runtime.insert_screenshot(2, ScreenshotState::pending(ScreenshotKind::Viewport));
            let mut capture = Box::pin(await_screenshot(
                &inner,
                &runtime,
                1,
                egui::ViewportId::ROOT,
                &ScreenshotKind::Viewport,
                0,
            ));
            poll_fn(|cx| {
                assert!(capture.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;

            let image = Arc::new(solid_image(2, 2));
            if arrives_before_cancellation {
                let mut state = ScreenshotState::pending(ScreenshotKind::Viewport);
                state.mark_ready(Arc::clone(&image));
                runtime.insert_screenshot(1, state);
            }
            drop(capture);

            assert!(runtime.screenshot_state(1).is_none());
            assert_eq!(Arc::strong_count(&image), 1);
            assert!(runtime.screenshot_state(2).is_some());
        }
    }

    #[tokio::test]
    async fn completed_screenshot_returns_image_and_releases_request() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        let image = Arc::new(solid_image(2, 2));
        let mut state = ScreenshotState::pending(ScreenshotKind::Viewport);
        state.mark_ready(Arc::clone(&image));
        runtime.insert_screenshot(1, state);

        let captured = await_screenshot(
            &inner,
            &runtime,
            1,
            egui::ViewportId::ROOT,
            &ScreenshotKind::Viewport,
            0,
        )
        .await
        .expect("completed capture");

        assert!(Arc::ptr_eq(&captured.image().expect("image"), &image));
        assert!(runtime.screenshot_state(1).is_none());
    }

    #[tokio::test]
    async fn a_pending_screenshot_completes_when_its_callback_answers() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        runtime.insert_screenshot(1, ScreenshotState::pending(ScreenshotKind::Viewport));
        let callback = runtime.screenshot_callback(&inner, 1, egui::ViewportId::ROOT);
        let mut capture = Box::pin(await_screenshot(
            &inner,
            &runtime,
            1,
            egui::ViewportId::ROOT,
            &ScreenshotKind::Viewport,
            0,
        ));
        poll_fn(|cx| {
            assert!(capture.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        let image = Arc::new(solid_image(2, 2));
        callback.complete(Arc::clone(&image));
        let captured = capture.await.expect("answered capture");

        assert!(Arc::ptr_eq(&captured.image().expect("image"), &image));
        assert!(runtime.screenshot_state(1).is_none());
    }

    #[tokio::test]
    async fn a_dropped_screenshot_callback_fails_the_capture() {
        let inner = Arc::new(Inner::new());
        let runtime = Runtime::ensure_for_inner(&inner);
        runtime.insert_screenshot(1, ScreenshotState::pending(ScreenshotKind::Viewport));
        let callback = runtime.screenshot_callback(&inner, 1, egui::ViewportId::ROOT);
        let mut capture = Box::pin(await_screenshot(
            &inner,
            &runtime,
            1,
            egui::ViewportId::ROOT,
            &ScreenshotKind::Viewport,
            0,
        ));
        poll_fn(|cx| {
            assert!(capture.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(callback);
        let error = capture.await.expect_err("dropped capture");

        assert!(error.message().contains("dropped the screenshot request"));
        assert!(runtime.screenshot_state(1).is_none());
    }

    fn solid_image(width: usize, height: usize) -> egui::ColorImage {
        egui::ColorImage {
            size: [width, height],
            source_size: egui::Vec2::new(width as f32, height as f32),
            pixels: vec![egui::Color32::WHITE; width * height],
        }
    }

    /// Build an image whose left half is black and right half is white.
    fn split_image(width: usize, height: usize) -> egui::ColorImage {
        let mut pixels = Vec::with_capacity(width * height);
        for _ in 0..height {
            for x in 0..width {
                pixels.push(if x < width / 2 {
                    egui::Color32::BLACK
                } else {
                    egui::Color32::WHITE
                });
            }
        }
        egui::ColorImage {
            size: [width, height],
            source_size: egui::Vec2::new(width as f32, height as f32),
            pixels,
        }
    }

    #[test]
    fn scale_screenshot_image_leaves_small_images_unchanged() {
        let image = solid_image(800, 600);
        let scaled = scale_screenshot_image(&image, DEFAULT_MAX_SCREENSHOT_DIMENSION);
        assert_eq!(scaled.size, [800, 600]);
    }

    #[test]
    fn scale_screenshot_image_caps_the_long_edge() {
        let image = solid_image(3200, 1800);
        let scaled = scale_screenshot_image(&image, 1600);
        assert_eq!(scaled.size, [1600, 900]);
    }

    #[test]
    fn scale_screenshot_image_keeps_the_captured_size_when_the_cap_is_zero() {
        let image = solid_image(3200, 1800);
        let scaled = scale_screenshot_image(&image, 0);
        assert_eq!(scaled.size, [3200, 1800]);
    }

    #[test]
    fn scale_screenshot_image_averages_the_source_pixels() {
        // A point sample would return pure black or pure white. An area
        // average returns the midpoint, which keeps reduced text readable.
        let image = split_image(8, 2);
        let scaled = scale_screenshot_image(&image, 1);
        assert_eq!(scaled.size, [1, 1]);
        let [red, green, blue, _] = scaled.pixels[0].to_array();
        assert!((120..=136).contains(&red), "unexpected red {red}");
        assert_eq!([red, green, blue], [red; 3]);
    }

    #[test]
    fn scale_screenshot_image_averages_large_sources_without_overflow() {
        // A tiny thumbnail of a high-resolution capture can sum more than
        // u32::MAX in each channel, even though every source pixel is valid.
        let image = solid_image(5120, 3456);
        let scaled = scale_screenshot_image(&image, 1);
        assert_eq!(scaled.size, [1, 1]);
        assert_eq!(scaled.pixels, vec![egui::Color32::WHITE]);
    }

    #[test]
    fn encode_screenshot_reports_the_selected_media_type() {
        let image = solid_image(4, 4);
        let jpeg = encode_screenshot(&image, ScreenshotOptions::default()).expect("jpeg");
        assert_eq!(jpeg.media_type, "image/jpeg");
        let png = encode_screenshot(
            &image,
            ScreenshotOptions {
                format: ScreenshotFormat::Png,
                max_dimension: 0,
            },
        )
        .expect("png");
        assert_eq!(png.media_type, "image/png");
        assert_ne!(jpeg.data, png.data);
    }

    #[test]
    fn png_screenshots_preserve_translucent_colors() {
        for (color, expected) in [
            (egui::Color32::TRANSPARENT, [0, 0, 0, 0]),
            (
                egui::Color32::from_rgba_premultiplied(64, 32, 16, 128),
                [128, 64, 32, 128],
            ),
            (egui::Color32::from_rgb(12, 34, 56), [12, 34, 56, 255]),
        ] {
            let mut image = solid_image(16, 16);
            image.pixels.fill(color);
            let encoded = encode_screenshot(
                &image,
                ScreenshotOptions {
                    format: ScreenshotFormat::Png,
                    max_dimension: 0,
                },
            )
            .expect("encode PNG");
            let bytes = STANDARD.decode(encoded.data).expect("decode base64");
            let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .expect("decode PNG")
                .to_rgba8();
            assert_eq!(decoded.dimensions(), (16, 16));
            assert!(
                decoded.pixels().all(|pixel| pixel.0 == expected),
                "PNG must use straight alpha for {color:?}"
            );
        }
    }

    #[test]
    fn jpeg_screenshots_composite_translucent_colors_once() {
        for (color, expected) in [
            (egui::Color32::TRANSPARENT, [255_u8, 255, 255]),
            (
                egui::Color32::from_rgba_premultiplied(64, 32, 16, 128),
                [191, 159, 143],
            ),
            (egui::Color32::from_rgb(12, 34, 56), [12, 34, 56]),
        ] {
            let mut image = solid_image(16, 16);
            image.pixels.fill(color);
            let encoded = encode_screenshot(
                &image,
                ScreenshotOptions {
                    format: ScreenshotFormat::Jpeg,
                    max_dimension: 0,
                },
            )
            .expect("encode JPEG");
            let bytes = STANDARD.decode(encoded.data).expect("decode base64");
            let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
                .expect("decode JPEG")
                .to_rgb8();
            assert_eq!(decoded.dimensions(), (16, 16));
            for pixel in decoded.pixels() {
                for (actual, expected) in pixel.0.into_iter().zip(expected) {
                    assert!(
                        actual.abs_diff(expected) <= 3,
                        "JPEG white composite for {color:?}: expected {expected}, got {actual}"
                    );
                }
            }
        }
    }

    #[test]
    fn screenshot_format_parses_only_supported_words() {
        assert_eq!(
            ScreenshotFormat::parse("jpeg"),
            Some(ScreenshotFormat::Jpeg)
        );
        assert_eq!(ScreenshotFormat::parse("png"), Some(ScreenshotFormat::Png));
        assert_eq!(ScreenshotFormat::parse("webp"), None);
    }

    #[test]
    fn default_screenshot_options_keep_the_established_encoding() {
        let options = ScreenshotOptions::default();
        assert_eq!(options.format, ScreenshotFormat::Jpeg);
        assert_eq!(options.max_dimension, DEFAULT_MAX_SCREENSHOT_DIMENSION);
    }
}
