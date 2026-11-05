//! Tests of screenshots, native captures, and pixel samples.

use super::*;

#[test]
fn native_capture_crop_uses_bottom_center_content_region() {
    let pixels = (0..16).map(egui::Color32::from_gray).collect::<Vec<_>>();
    let image = egui::ColorImage {
        size: [4, 4],
        source_size: egui::Vec2::new(4.0, 4.0),
        pixels,
    };
    let snapshot = viewport_snapshot_with_inner_size(Vec2 { x: 2.0, y: 2.0 });

    let cropped = crop_native_capture_to_viewport(image, &snapshot).expect("crop");

    assert_eq!(cropped.size, [2, 2]);
    assert_eq!(
        cropped.pixels,
        vec![
            egui::Color32::from_gray(9),
            egui::Color32::from_gray(10),
            egui::Color32::from_gray(13),
            egui::Color32::from_gray(14),
        ]
    );
}

#[test]
fn native_capture_crop_rejects_smaller_capture() {
    let image = egui::ColorImage {
        size: [1, 2],
        source_size: egui::Vec2::new(1.0, 2.0),
        pixels: vec![egui::Color32::BLACK; 2],
    };
    let snapshot = viewport_snapshot_with_inner_size(Vec2 { x: 2.0, y: 2.0 });

    let error = crop_native_capture_to_viewport(image, &snapshot).expect_err("too small");

    assert!(error.contains("smaller than viewport content"));
}

#[test]
fn root_screenshot_timeout_message_does_not_claim_child_fallback() {
    let message = screenshot_timeout_message(egui::ViewportId::ROOT, "fallback unavailable");

    assert!(message.contains("Native screenshot fallback is only available"));
    assert!(!message.contains("attempted for this child viewport"));
}

#[test]
fn native_screenshot_fallback_waits_for_the_frame_that_answers_the_callback() {
    let child = egui::ViewportId::from_hash_of("child");

    assert!(!should_try_native_screenshot_fallback(child, 0));
    assert!(!should_try_native_screenshot_fallback(child, 1));
    assert_eq!(
        should_try_native_screenshot_fallback(child, 2),
        cfg!(target_os = "macos")
    );
    assert!(!should_try_native_screenshot_fallback(
        egui::ViewportId::ROOT,
        5
    ));
}

#[test]
fn sample_color_image_converts_logical_points_and_preserves_alpha() {
    let image = egui::ColorImage::new(
        [2, 2],
        vec![
            egui::Color32::BLACK,
            egui::Color32::from_rgba_premultiplied(0x11, 0x22, 0x33, 0x44),
            egui::Color32::WHITE,
            egui::Color32::from_rgb(0xaa, 0xbb, 0xcc),
        ],
    );

    let samples = sample_color_image(&image, 2.0, &[Pos2 { x: 0.75, y: 0.25 }]).expect("sample");

    assert_eq!(samples[0].physical, [1, 0]);
    assert_eq!(samples[0].rgba, [0x11, 0x22, 0x33, 0x44]);
    assert_eq!(samples[0].hex, "#11223344");
}

#[test]
fn sample_color_image_rejects_out_of_bounds_positions() {
    let image = egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]);

    let error =
        sample_color_image(&image, 1.0, &[Pos2 { x: 1.0, y: 0.0 }]).expect_err("out of bounds");

    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(error.message().contains("outside"));
}

#[test]
fn sample_widget_pixels_uses_relative_positions() {
    let mut pixels = vec![egui::Color32::BLACK; 100];
    pixels[4 * 10 + 3] = egui::Color32::from_rgb(0x33, 0x66, 0x99);
    let image = egui::ColorImage::new([10, 10], pixels);
    let widget = make_entry_with_rect(
        "canvas",
        1,
        WidgetRole::Image,
        Rect {
            min: Pos2 { x: 2.0, y: 3.0 },
            max: Pos2 { x: 6.0, y: 8.0 },
        },
        None,
    );

    let samples = sample_widget_relative_pixels(
        &image,
        1.0,
        &widget,
        Some("root"),
        &[Pos2 { x: 1.0, y: 1.0 }],
    )
    .expect("sample");

    assert_eq!(samples[0].position, Pos2 { x: 1.0, y: 1.0 });
    assert_eq!(samples[0].physical, [3, 4]);
    assert_eq!(samples[0].hex, "#336699ff");
}

#[test]
fn sample_widget_pixels_reports_widget_bounds() {
    let image = egui::ColorImage::new([10, 10], vec![egui::Color32::BLACK; 100]);
    let widget = make_entry_with_rect(
        "canvas",
        1,
        WidgetRole::Image,
        Rect {
            min: Pos2 { x: 2.0, y: 3.0 },
            max: Pos2 { x: 6.0, y: 8.0 },
        },
        None,
    );

    let error = sample_widget_relative_pixels(
        &image,
        1.0,
        &widget,
        Some("root"),
        &[Pos2 { x: 4.0, y: 0.0 }],
    )
    .expect_err("out of bounds");

    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details().expect("details")["widget_id"], "canvas");
    assert_eq!(error.details().expect("details")["viewport_name"], "root");
}

#[test]
fn sample_widget_grid_uses_visible_rect_and_single_axis_centers() {
    let image = egui::ColorImage::new([20, 20], vec![egui::Color32::WHITE; 400]);
    let mut widget = make_entry_with_rect(
        "canvas",
        1,
        WidgetRole::Image,
        Rect {
            min: Pos2 { x: 2.0, y: 2.0 },
            max: Pos2 { x: 12.0, y: 12.0 },
        },
        None,
    );
    widget.layout = Some(WidgetLayout {
        desired_size: Vec2 { x: 10.0, y: 10.0 },
        actual_size: Vec2 { x: 10.0, y: 10.0 },
        clip_rect: Rect {
            min: Pos2 { x: 4.0, y: 5.0 },
            max: Pos2 { x: 10.0, y: 9.0 },
        },
        clipped: true,
        overflow: false,
        available_rect: widget.rect,
        visible_fraction: 0.25,
        text: None,
    });

    let samples = sample_widget_grid(&image, 1.0, &widget, Some("root"), 1, 2).expect("grid");

    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0].position.x, 5.0);
    assert_eq!(samples[0].position.y, 4.0);
    assert_eq!(samples[1].position.x, 5.0);
    assert_eq!(samples[1].position.y, 6.0);
}

#[test]
fn sample_widget_grid_rejects_hidden_widgets() {
    let image = egui::ColorImage::new([10, 10], vec![egui::Color32::WHITE; 100]);
    let mut widget = make_entry("canvas", 1, WidgetRole::Image);
    widget.visible = false;

    let error =
        sample_widget_grid(&image, 1.0, &widget, Some("root"), 2, 2).expect_err("hidden widget");

    assert_eq!(error.code(), ErrorCode::NotActionable);
}
