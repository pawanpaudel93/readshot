//! Snapshot tests for the annotation renderer.
//!
//! These tests are intentionally written before the renderer body exists
//! (Task 3 precedes Task 4 in the plan). Each test:
//!
//! 1. Builds an annotation model exercising one variant — or two
//!    composites combining several variants.
//! 2. Calls [`readshot_core::render`] over the canonical 256×256 base
//!    canvas from `readshot-test-fixtures`.
//! 3. PNG-encodes the result and asserts it via
//!    `insta::assert_binary_snapshot!`.
//!
//! Until Task 4 lands, every call to `render` panics in `unimplemented!()`
//! — every test below fails. That failure is the green light to begin
//! writing the renderer.
//!
//! Once Task 4 ships, run `cargo insta review` once to accept the
//! generated `.snap.png` files. Future renderer changes that produce
//! different bytes will cause an `insta` diff that the developer reviews
//! and accepts (or rejects, indicating a regression).

use std::io::Cursor;

use image::ImageFormat;
use readshot_core::{render, Annotation, PointLike, RectLike, Rgba};
use readshot_test_fixtures::base_256;

fn red() -> Rgba {
    Rgba::new(1.0, 0.0, 0.0, 1.0)
}

fn green() -> Rgba {
    Rgba::new(0.0, 0.7, 0.0, 1.0)
}

fn blue() -> Rgba {
    Rgba::new(0.1, 0.4, 1.0, 1.0)
}

fn yellow_translucent() -> Rgba {
    Rgba::new(1.0, 0.95, 0.0, 0.4)
}

fn render_png(model: Vec<Annotation>) -> Vec<u8> {
    let base = base_256();
    let out = render(&base, &model);
    let mut bytes = Vec::new();
    out.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("png encode");
    bytes
}

#[test]
fn rectangle() {
    let png = render_png(vec![Annotation::Rectangle {
        rect: RectLike::new(40.0, 40.0, 176.0, 176.0),
        color: red(),
        line_width: 4.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn ellipse() {
    let png = render_png(vec![Annotation::Ellipse {
        rect: RectLike::new(48.0, 80.0, 160.0, 96.0),
        color: green(),
        line_width: 3.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn line() {
    let png = render_png(vec![Annotation::Line {
        a: PointLike::new(20.0, 20.0),
        b: PointLike::new(236.0, 236.0),
        color: blue(),
        line_width: 3.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn arrow() {
    let png = render_png(vec![Annotation::Arrow {
        a: PointLike::new(40.0, 200.0),
        b: PointLike::new(216.0, 56.0),
        color: red(),
        line_width: 4.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn pen() {
    let png = render_png(vec![Annotation::Pen {
        points: vec![
            PointLike::new(40.0, 128.0),
            PointLike::new(80.0, 96.0),
            PointLike::new(120.0, 144.0),
            PointLike::new(160.0, 80.0),
            PointLike::new(216.0, 160.0),
        ],
        color: blue(),
        line_width: 3.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn highlighter() {
    let png = render_png(vec![Annotation::Highlighter {
        points: vec![PointLike::new(20.0, 128.0), PointLike::new(236.0, 128.0)],
        color: yellow_translucent(),
        line_width: 16.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn text() {
    let png = render_png(vec![Annotation::Text {
        content: "Hello, Readshot".to_string(),
        origin: PointLike::new(32.0, 140.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".to_string(),
        size: 24.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn blur() {
    let png = render_png(vec![Annotation::Blur {
        rect: RectLike::new(48.0, 48.0, 160.0, 160.0),
        radius: 8.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn pixelate() {
    let png = render_png(vec![Annotation::Pixelate {
        rect: RectLike::new(48.0, 48.0, 160.0, 160.0),
        block_size: 16.0,
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn numbered_pin() {
    let png = render_png(vec![Annotation::NumberedPin {
        origin: PointLike::new(128.0, 128.0),
        number: 7,
        color: red(),
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn crop() {
    // Cropping selects a rectangular sub-region of the base image. The
    // resulting snapshot is smaller than the others (128×128 here).
    let png = render_png(vec![Annotation::Crop {
        rect: RectLike::new(64.0, 64.0, 128.0, 128.0),
    }]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn composite_arrow_text_blur() {
    // A typical "redact + annotate" scene: blur a sensitive region, draw
    // an arrow at it, label the arrow with text.
    let png = render_png(vec![
        Annotation::Blur {
            rect: RectLike::new(140.0, 140.0, 80.0, 60.0),
            radius: 6.0,
        },
        Annotation::Arrow {
            a: PointLike::new(40.0, 60.0),
            b: PointLike::new(150.0, 150.0),
            color: red(),
            line_width: 4.0,
        },
        Annotation::Text {
            content: "redacted".to_string(),
            origin: PointLike::new(20.0, 56.0),
            color: red(),
            font_family: "system-ui".to_string(),
            size: 18.0,
        },
    ]);
    insta::assert_binary_snapshot!(".png", png);
}

#[test]
fn composite_pen_highlight_pin() {
    // A teaching scene: highlighter underline, freehand circle, two
    // numbered pins. Stresses the renderer's z-order — pins must end up
    // on top.
    let png = render_png(vec![
        Annotation::Highlighter {
            points: vec![PointLike::new(20.0, 200.0), PointLike::new(236.0, 200.0)],
            color: yellow_translucent(),
            line_width: 14.0,
        },
        Annotation::Pen {
            points: vec![
                PointLike::new(60.0, 60.0),
                PointLike::new(80.0, 40.0),
                PointLike::new(120.0, 50.0),
                PointLike::new(140.0, 80.0),
                PointLike::new(120.0, 110.0),
                PointLike::new(80.0, 110.0),
                PointLike::new(60.0, 80.0),
                PointLike::new(60.0, 60.0),
            ],
            color: blue(),
            line_width: 3.0,
        },
        Annotation::NumberedPin {
            origin: PointLike::new(100.0, 75.0),
            number: 1,
            color: red(),
        },
        Annotation::NumberedPin {
            origin: PointLike::new(180.0, 200.0),
            number: 2,
            color: red(),
        },
    ]);
    insta::assert_binary_snapshot!(".png", png);
}
