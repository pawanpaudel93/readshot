//! SC-16 corpus: forward scrolling over text pages must stitch exactly
//! the page it showed, through the live judge and the stitcher alike,
//! for uniform steps, mixed steps and a flick of more than half the
//! region. Ported from the W5e review probe (fixed seeds, no I/O).

use super::*;
use image::{Rgba, RgbaImage};
use readshot_core::scroll_stitch::{stitch_scrolling, StitchConfig};

/// Frame width and height: small enough for a debug-build test, with
/// the probe's text geometry (36 px line pitch, 20 px x-height).
const W: u32 = 200;
const VH: u32 = 240;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn r(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, c: u8) {
    for yy in y..(y + h).min(img.height()) {
        for xx in x..(x + w).min(img.width()) {
            img.put_pixel(xx, yy, Rgba([c, c, c, 255]));
        }
    }
}

/// A glyph-ish text line: words of stems and bars.
fn text_line(img: &mut RgbaImage, rng: &mut Rng, y: u32, x1: u32) {
    let mut x = 20;
    while x < x1 {
        for _ in 0..3 + rng.r(8) {
            let gw = 6 + rng.r(8);
            if x + gw > x1 {
                return;
            }
            let asc = if rng.r(4) == 0 { 6 } else { 0 };
            fill(img, x, y - asc, 2 + rng.r(3), 20 + asc, 20);
            fill(img, x + gw / 2, y + rng.r(10), gw / 2, 2 + rng.r(3), 20);
            x += gw + 2;
        }
        x += 10;
    }
}

/// A page of paragraphs: 3–8 lines on a 36 px pitch, ragged last line.
fn text_page(h: u32, seed: u64) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(W, h, Rgba([255, 255, 255, 255]));
    let mut rng = Rng(seed);
    let mut y = 40;
    while y + 40 < h {
        let lines = 3 + rng.r(6);
        for i in 0..lines {
            if y + 40 >= h {
                break;
            }
            let x1 = if i == lines - 1 {
                40 + rng.r(W - 80)
            } else {
                W - 20
            };
            text_line(&mut img, &mut rng, y, x1);
            y += 36;
        }
        y += 30;
    }
    img
}

fn frames_at(page: &RgbaImage, positions: &[u32]) -> Vec<RgbaImage> {
    positions
        .iter()
        .map(|&p| image::imageops::crop_imm(page, 0, p, W, VH).to_image())
        .collect()
}

/// The live judge, as the session runs it: each frame against the last
/// accepted one. The indexes of the frames it keeps.
fn judged(frames: &[RgbaImage]) -> Vec<usize> {
    let mut kept = vec![0];
    for (i, f) in frames.iter().enumerate().skip(1) {
        if frame_has_scroll_motion(&frames[*kept.last().unwrap()], f) {
            kept.push(i);
        }
    }
    kept
}

/// Stitch `frames` and require exactly the page from the first frame's
/// top to the bottom of the frame at `positions[last]`.
fn assert_stitches(what: &str, page: &RgbaImage, top: u32, last: u32, frames: &[RgbaImage]) {
    let out = stitch_scrolling(frames, StitchConfig::default()).unwrap();
    let want = image::imageops::crop_imm(page, 0, top, W, last + VH - top).to_image();
    assert_eq!(out.height(), want.height(), "{what}: height");
    assert!(out == want, "{what}: pixels differ");
}

/// The full pipeline: the judge never skips two frames in a row (it
/// never stalls; a single skip is a sparse strip tying a short move with
/// the true one, as it always has), and the kept frames stitch exactly
/// the page up to the last one kept.
fn assert_pipeline(
    what: &str,
    page: &RgbaImage,
    positions: &[u32],
    frames: &[RgbaImage],
) -> Vec<usize> {
    let kept = judged(frames);
    let mut ends = kept.clone();
    ends.push(frames.len());
    assert!(
        ends.windows(2).all(|w| w[1] - w[0] <= 2),
        "{what}: the judge skipped two frames in a row (kept {kept:?} of {})",
        frames.len()
    );
    let kept_frames: Vec<RgbaImage> = kept.iter().map(|&i| frames[i].clone()).collect();
    let last = positions[*kept.last().unwrap()];
    assert_stitches(
        &format!("{what} (pipeline)"),
        page,
        positions[0],
        last,
        &kept_frames,
    );
    kept
}

/// Uniform steps from 10 px to two thirds of the region, over several
/// pages: the stitcher alone on every frame, and the full pipeline.
#[test]
fn uniform_forward_scrolls_over_text_stitch_the_exact_page() {
    for seed in [7u64, 11, 17] {
        let page = text_page(1600, seed);
        for step in (10..=VH * 2 / 3).step_by(15) {
            // At most 8 frames a case keeps the debug build quick.
            let positions: Vec<u32> = (0..=(1200 / step).min(7)).map(|i| i * step).collect();
            let frames = frames_at(&page, &positions);
            let what = format!("seed {seed} step {step}");
            let last = *positions.last().unwrap();
            assert_stitches(&format!("{what} (stitcher)"), &page, 0, last, &frames);
            assert_pipeline(&what, &page, &positions, &frames);
        }
    }
}

/// Mixed steps (slow, medium, up to two thirds of the region) through
/// the full pipeline.
#[test]
fn mixed_forward_scrolls_over_text_stitch_the_exact_page() {
    for seed in [3u64, 5, 23] {
        let page = text_page(1600, seed);
        let mut rng = Rng(seed * 77 + 1);
        let mut positions = vec![0u32];
        while *positions.last().unwrap() < 1100 {
            let step = match rng.r(3) {
                0 => 8 + rng.r(20),
                1 => 30 + rng.r(90),
                _ => 120 + rng.r(VH * 2 / 3 - 120),
            };
            positions.push(positions.last().unwrap() + step);
        }
        let frames = frames_at(&page, &positions);
        assert_pipeline(&format!("seed {seed}"), &page, &positions, &frames);
    }
}

/// The stitcher's strip height at this frame size (a quarter of it),
/// so the largest move whose frames still overlap is `VH - STRIP`.
const STRIP: u32 = VH / 4;

/// One flick between slow scrolling: more than half the region, up to
/// two thirds (the judge's own template), and on into the band between
/// two thirds and the last overlap (`VH - STRIP`), where only the
/// stitcher's bottom strip still matches. On a 600 px region that band
/// is 400–520 px; 420, 450, 500 and 515 px scale to 163, 168, 176 and
/// 179 here. The frame after the flick is placed, and so is everything
/// after it.
#[test]
fn a_flick_of_more_than_half_the_region_does_not_stop_the_capture() {
    let page = text_page(1200, 7);
    let band = |on_600: u32| {
        let (lo, hi) = (VH * 2 / 3, VH - STRIP);
        lo + (on_600 - 400) * (hi - lo) / 120
    };
    let flicks = [
        VH / 2 + 10,
        VH / 2 + 25,
        VH * 2 / 3,
        band(420),
        band(450),
        band(500),
        band(515),
        VH - STRIP,
    ];
    assert_eq!(&flicks[3..7], &[163, 168, 176, 179]);
    for flick in flicks {
        let mut positions: Vec<u32> = (0..6).map(|i| i * 20).collect();
        let base = 100 + flick;
        positions.extend((0..6).map(|i| base + i * 20));
        let frames = frames_at(&page, &positions);
        let what = format!("flick {flick}");
        let kept = assert_pipeline(&what, &page, &positions, &frames);
        assert_eq!(kept.len(), frames.len(), "{what}: every frame is kept");
    }
}

/// A jump past any overlap (more than `VH - STRIP`) can't be placed:
/// the frames after it are reported unplaced (the window then says the
/// scroll was too fast), nothing is stitched in, and scrolling back up
/// into range joins again exactly.
#[test]
fn a_jump_past_any_overlap_is_reported_and_never_stitched_in() {
    use crate::runtime::scroll::{scroll_motion, ScrollMotion};
    let page = text_page(1400, 11);
    let mut positions: Vec<u32> = (0..4).map(|i| i * 20).collect();
    let jump = 60 + VH; // no shared rows with the frame at 60
    positions.extend([jump, jump + 20, 60 + VH - STRIP - 20, 60 + VH - STRIP]);
    let frames = frames_at(&page, &positions);
    assert_eq!(
        scroll_motion(&frames[3], &frames[4]),
        ScrollMotion::Unplaced
    );
    assert_eq!(
        scroll_motion(&frames[3], &frames[5]),
        ScrollMotion::Unplaced
    );
    let kept = judged(&frames);
    assert_eq!(
        kept,
        vec![0, 1, 2, 3, 6, 7],
        "back in range, it joins again"
    );
    let kept_frames: Vec<RgbaImage> = kept.iter().map(|&i| frames[i].clone()).collect();
    assert_stitches("jump", &page, 0, positions[7], &kept_frames);
}

/// A large scroll back up is judged Back, never Unplaced (so it can't
/// show "Scrolled too fast") and never Down: a 300 px flick up and
/// repeated 100 px steps up on a 600 px region (120 and 40 px here),
/// past the judge template's upward window (50 px here) but within the
/// overlap with the last accepted frame.
#[test]
fn a_large_scroll_back_up_is_judged_back() {
    use crate::runtime::scroll::{scroll_motion, ScrollMotion};
    for seed in [7u64, 11] {
        let page = text_page(1400, seed);
        let anchor = frames_at(&page, &[400]).remove(0);
        for up in [120u32, 40, 80, 120, 160] {
            let frame = frames_at(&page, &[400 - up]).remove(0);
            assert_eq!(
                scroll_motion(&anchor, &frame),
                ScrollMotion::Back,
                "seed {seed}: back up {up}"
            );
        }
    }
}

/// A still page with something animated across the judge's template
/// strip (a video, a spinner) is a still page, not an unplaced frame:
/// the frame is not motion and doesn't count towards the hint.
#[test]
fn an_animated_element_on_a_still_page_is_still() {
    use crate::runtime::scroll::{scroll_motion, ScrollMotion};
    let page = text_page(1400, 7);
    let base = frames_at(&page, &[300]).remove(0);
    let mut rng = Rng(99);
    // (top, height, left, right): across the template strip, high up,
    // and a full-width video along the bottom, where the stitcher's own
    // strip is.
    for (top, height, left, right) in [
        (140u32, 32u32, 60u32, 140u32),
        (150, 32, 60, 140),
        (30, 32, 60, 140),
        (200, 32, 60, 140),
        (170, 70, 0, W),
    ] {
        let top_ = top;
        let mut frame = base.clone();
        for y in top..top + height {
            for x in left..right {
                let v = rng.r(256) as u8;
                frame.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        assert_eq!(
            scroll_motion(&base, &frame),
            ScrollMotion::Down(0),
            "box at y {top_}, {height} rows"
        );
    }
}

/// The window's "Scrolled too fast" hint over whole sequences, counted
/// as the session counts it: shown from the second frame in a row that
/// no offset places (`counts_as_unplaced`), reset by any other verdict.
fn hint_shown(frames: &[RgbaImage]) -> Vec<bool> {
    use crate::runtime::scroll::{
        counts_as_unplaced, scroll_motion, ScrollMotion, SCROLL_TOO_FAST_AFTER,
    };
    let (mut anchor, mut refused, mut count) = (0usize, None::<usize>, 0u32);
    let mut shown = vec![false];
    for (i, f) in frames.iter().enumerate().skip(1) {
        let motion = scroll_motion(&frames[anchor], f);
        if matches!(motion, ScrollMotion::Down(dy) if dy >= SCROLL_MIN_ACCEPTED_MOTION_PX) {
            (anchor, refused, count) = (i, None, 0);
        } else {
            let unplaced = counts_as_unplaced(motion, refused.map(|r| &frames[r]), f);
            count = if unplaced { count + 1 } else { 0 };
            refused = Some(i);
        }
        shown.push(count >= SCROLL_TOO_FAST_AFTER);
    }
    shown
}

/// The hint shows after a real jump past any overlap, and never on a
/// still page with an animation, a 300 px flick up, repeated 100 px
/// steps up, or a long slow scroll back up past the overlap with the
/// last accepted frame (each frame still places backward against the
/// one before it).
#[test]
fn the_too_fast_hint_shows_only_after_a_jump_past_any_overlap() {
    let page = text_page(2000, 11);
    let down: Vec<u32> = (0..=10).map(|i| i * 40).collect();
    let with = |tail: &[u32]| {
        let mut p = down.clone();
        p.extend_from_slice(tail);
        frames_at(&page, &p)
    };
    let none = |what: &str, frames: &[RgbaImage]| {
        let shown = hint_shown(frames);
        assert!(!shown.contains(&true), "{what}: hint at {shown:?}");
    };
    none("flick up 120 then still", &with(&[280, 280, 280, 280]));
    none(
        "up 40 x4 then down",
        &with(&[360, 320, 280, 240, 280, 320, 360, 400, 440]),
    );
    let slow_back: Vec<u32> = (1..=20).map(|i| 400 - i * 16).collect();
    none("slow scroll back up past the overlap", &with(&slow_back));
    let mut still = vec![frames_at(&page, &[300]).remove(0); 8];
    let mut rng = Rng(5);
    for f in still.iter_mut().skip(1) {
        for y in 145..177 {
            for x in 60..140 {
                let v = rng.r(256) as u8;
                f.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
    }
    none("still page with an animation", &still);

    let jump = 400 + VH + 60;
    let frames = with(&[jump, jump + 20, jump + 40, jump + 60]);
    let shown = hint_shown(&frames);
    assert_eq!(
        &shown[11..],
        &[false, true, true, true],
        "jump past any overlap: from the second unplaced frame"
    );
    // On this page, frames after a 250 px jump line their text lines up
    // with the anchor's well enough to stand out as a scroll back up,
    // but not exactly: they are still unplaced, and the hint stays.
    let page = text_page(2000, 7);
    let mut p = down.clone();
    p.extend([650, 670, 690, 710, 730]);
    let shown = hint_shown(&frames_at(&page, &p));
    assert_eq!(
        &shown[11..],
        &[false, true, true, true, true],
        "a look-alike back match never hides the hint"
    );
}
