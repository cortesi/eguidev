//! Diagnostic-provider error adaptation.

use serde_json::json;

use super::{ErrorCode, ScriptErrorInfo, ScriptLocation, *};

pub(super) fn error_info(
    error: eguidev::DiagnosticError,
    location: Option<ScriptLocation>,
) -> ScriptErrorInfo {
    let timed_out = error.code == "timeout";
    ScriptErrorInfo {
        error_type: if timed_out { "timeout" } else { "diagnostic" }.to_string(),
        message: error.message,
        location,
        backtrace: None,
        code: Some(if timed_out {
            "timeout".to_string()
        } else {
            ErrorCode::DiagnosticFailed.as_str().to_string()
        }),
        details: Some(json!({
            "cause_code": error.code,
            "details": error.details,
        })),
    }
}

impl DevMcpServer {
    pub(super) async fn show_highlight(
        &self,
        viewport_id: Option<String>,
        target: Option<WidgetRef>,
        rect: Option<Rect>,
        color: String,
    ) -> ToolResult<OverlayHighlightResult> {
        let (rect, key) = if let Some(ref target) = target {
            let widget = resolve_widget(&self.inner, viewport_id.as_deref(), target)?;
            (widget.interact_rect, format!("widget:{}", widget.id))
        } else if let Some(rect) = rect {
            let key = format!(
                "rect:{},{},{},{}",
                rect.min.x, rect.min.y, rect.max.x, rect.max.y
            );
            (rect, key)
        } else {
            return Err(
                ToolError::new(ErrorCode::InvalidArgument, "Missing rect or target").into(),
            );
        };
        let color = parse_color(&color).ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidArgument,
                format!("Invalid color: {color}"),
            )
        })?;
        self.inner.set_overlay(
            key,
            OverlayEntry {
                rect: egui::Rect::from(rect),
                color,
                stroke_width: 2.0,
            },
        );
        Ok(OverlayHighlightResult { rect })
    }

    /// Hide highlights. If a target widget is given, removes just that widget's
    /// highlight. Otherwise clears all highlights.
    pub(super) async fn clear_highlights(
        &self,
        viewport_id: Option<String>,
        target: Option<WidgetRef>,
    ) -> ToolResult<()> {
        if let Some(ref target) = target {
            let widget = resolve_widget(&self.inner, viewport_id.as_deref(), target)?;
            self.inner.remove_overlay(&format!("widget:{}", widget.id));
        } else {
            self.inner.clear_overlays();
        }
        Ok(())
    }

    /// Enable the persistent debug overlay with a fresh configuration.
    pub(super) async fn show_debug_overlay(
        &self,
        _viewport_id: Option<String>,
        mode: Option<OverlayDebugModeName>,
        scope: Option<WidgetRef>,
        options: Option<OverlayDebugOptionsInput>,
    ) -> ToolResult<()> {
        let mut config = OverlayDebugConfig {
            enabled: true,
            mode: mode.map(Into::into).unwrap_or(OverlayDebugMode::Bounds),
            scope,
            options: OverlayDebugOptions::default(),
        };
        if let Some(input) = options {
            apply_overlay_debug_options(&mut config.options, input)?;
        }
        self.inner.set_overlay_debug_config(config);
        Ok(())
    }

    /// Disable the persistent debug overlay.
    pub(super) async fn clear_debug_overlay(&self) -> ToolResult<()> {
        let config = OverlayDebugConfig::default();
        self.inner.set_overlay_debug_config(config);
        Ok(())
    }
}
