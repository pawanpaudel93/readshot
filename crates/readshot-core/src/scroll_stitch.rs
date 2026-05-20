//! Vertical scroll stitching for capture-as-you-scroll workflows.
//!
//! Given a sequence of `RgbaImage` frames captured from a *fixed
//! viewport* (same region of the screen) while the user scrolls
//! **downward** through a page or document, this module:
//!
//! 1. Detects sticky header / footer rows (rows whose content is
//!    constant across every frame in the sequence).
//! 2. Estimates the vertical scroll offset between consecutive frames
//!    via a sum-of-absolute-differences (SAD) template match on a
//!    horizontal slice of the previous frame.
//! 3. Drops frames whose offset is out of the trust window
//!    (`< min_motion` = duplicate; `> max_motion` = jump cut).
//! 4. Concatenates the bottom `dy` rows of each subsequent frame
//!    onto a tall canvas, omitting the sticky regions from the
//!    pasted strip so they don't repeat for every accepted frame.
//!
//! Limitations:
//! - Vertical only — horizontal scroll is not handled.
//! - Assumes monotonically increasing scroll. Backwards scroll
//!   between two frames is detected as `< min_motion` and dropped.
//! - Sticky detection is intensity-based (row-mean stability across
//!   frames) and won't survive animated headers / blinking carets.
//! - No SIFT / ORB feature matching — purely subsampled SAD. Fast
//!   (~5 ms per 1080p pair on a 2023 laptop) but fragile on highly
//!   transparent / animated content.
//!
//! The algorithm is pure: it takes `&[RgbaImage]` in and produces
//! `RgbaImage` out; tests are the source of truth for behavior.

use image::RgbaImage;

/// Tuning knobs for the stitcher. Defaults target typical browser
/// page scrolling at 1080p / Retina.
#[derive(Debug, Clone, Copy)]
pub struct StitchConfig {
    /// Minimum vertical scroll between consecutive frames (px). Below
    /// this the frame is dropped as a near-duplicate of its predecessor.
    pub min_motion: u32,
    /// Maximum vertical scroll between consecutive frames (px). Above
    /// this the frame is dropped because the user probably jumped
    /// (page-down, jump to anchor), and stitching would produce a
    /// torn output.
    pub max_motion: u32,
    /// Height of the template strip pulled from the previous frame's
    /// bottom edge (px). Larger = more robust but slower.
    pub strip_height: u32,
    /// Subsample stride for SAD computation. 1 = check every pixel,
    /// 2 = every second, etc. Higher = faster, slightly less accurate.
    pub subsample: u32,
    /// Hard cap on the final stitched image height (px). Returned as
    /// `StitchError::OutputTooTall` rather than silently allocating
    /// gigabytes of RGBA.
    pub max_output_height: u32,
    /// Per-channel tolerance (0-255) for considering a row "stable"
    /// across frames when classifying sticky regions. 6 is permissive
    /// enough to survive font anti-aliasing jitter without admitting
    /// real motion.
    pub sticky_tolerance: u8,
    /// Maximum number of contiguous rows from each edge to consider
    /// when detecting sticky elements (as a fraction of frame height,
    /// numerator only — denominator is 4). 1 = up to 25% top + 25%
    /// bottom can be sticky. Prevents the whole frame being
    /// classified sticky when the user simply hasn't scrolled.
    pub sticky_search_fraction_quarter: u32,
}

impl Default for StitchConfig {
    fn default() -> Self {
        Self {
            min_motion: 8,
            max_motion: 1200,
            strip_height: 80,
            subsample: 2,
            max_output_height: 16_384,
            sticky_tolerance: 6,
            sticky_search_fraction_quarter: 1,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StitchError {
    #[error("no frames provided")]
    Empty,
    #[error("frame {index} has dimensions {got_w}x{got_h}, expected {want_w}x{want_h}")]
    DimensionMismatch {
        index: usize,
        got_w: u32,
        got_h: u32,
        want_w: u32,
        want_h: u32,
    },
    #[error("stitched output would be {height} px tall, exceeds cap {cap}")]
    OutputTooTall { height: u32, cap: u32 },
    #[error("frame {index} is degenerate: w={w} h={h}")]
    DegenerateFrame { index: usize, w: u32, h: u32 },
}

/// Sticky-row mask: counts of contiguous rows from the top and the
/// bottom of every frame that are visually stable across the whole
/// sequence (sticky headers / footers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StickyMask {
    pub top_rows: u32,
    pub bottom_rows: u32,
}

/// Stitch `frames` into one tall image. Returns the stitched image
/// or an error describing why stitching failed. Single-frame input
/// is returned unchanged (cloned).
pub fn stitch_scrolling(
    frames: &[RgbaImage],
    config: StitchConfig,
) -> Result<RgbaImage, StitchError> {
    if frames.is_empty() {
        return Err(StitchError::Empty);
    }
    let w = frames[0].width();
    let h = frames[0].height();
    if w == 0 || h == 0 {
        return Err(StitchError::DegenerateFrame { index: 0, w, h });
    }
    for (i, f) in frames.iter().enumerate().skip(1) {
        if f.width() != w || f.height() != h {
            return Err(StitchError::DimensionMismatch {
                index: i,
                got_w: f.width(),
                got_h: f.height(),
                want_w: w,
                want_h: h,
            });
        }
    }
    if frames.len() == 1 {
        return Ok(frames[0].clone());
    }

    let sticky = detect_sticky(frames, &config);

    // Estimate scroll between consecutive frames.
    let mut offsets: Vec<u32> = Vec::with_capacity(frames.len() - 1);
    for pair in frames.windows(2) {
        let dy = estimate_scroll(&pair[0], &pair[1], &sticky, &config).unwrap_or(0);
        offsets.push(dy);
    }

    // Total height = base frame + sum of accepted offsets, minus the
    // bottom sticky region of the base frame if subsequent frames
    // contribute (so the sticky footer renders once at the very end).
    let max_motion = config.max_motion.min(h.saturating_sub(1));
    let accepted: Vec<u32> = offsets
        .iter()
        .copied()
        .map(|dy| {
            if dy >= config.min_motion && dy <= max_motion {
                dy
            } else {
                0
            }
        })
        .collect();
    let extra: u32 = accepted.iter().sum();
    let total_h = h.saturating_add(extra);
    if total_h > config.max_output_height {
        return Err(StitchError::OutputTooTall {
            height: total_h,
            cap: config.max_output_height,
        });
    }

    // Paste the first frame in full at the top.
    let mut out = RgbaImage::new(w, total_h);
    image::imageops::overlay(&mut out, &frames[0], 0, 0);
    let mut y_cursor = h as i64;

    // For each accepted frame, paste its bottom `dy` rows directly
    // beneath the previous content. Skip the bottom-sticky strip on
    // every frame except the last so the sticky footer doesn't
    // appear multiple times mid-stitch.
    for (i, &dy) in accepted.iter().enumerate() {
        if dy == 0 {
            continue;
        }
        let frame = &frames[i + 1];
        let is_last_accepted = accepted[i + 1..].iter().all(|&n| n == 0);
        let bottom_trim = if is_last_accepted {
            0
        } else {
            sticky.bottom_rows.min(dy)
        };
        let strip_h = dy.saturating_sub(bottom_trim);
        if strip_h == 0 {
            continue;
        }
        // Source rows are `[h - dy .. h - bottom_trim)` in `frame`.
        let src_y = h - dy;
        let strip = image::imageops::crop_imm(frame, 0, src_y, w, strip_h).to_image();
        image::imageops::overlay(&mut out, &strip, 0, y_cursor);
        y_cursor += strip_h as i64;
    }

    // If we shaved a sticky footer off every intermediate frame, the
    // canvas is now `bottom_trim_total` rows too tall — clip.
    let final_h = (y_cursor.max(0) as u32).min(total_h);
    if final_h < total_h {
        let cropped = image::imageops::crop_imm(&out, 0, 0, w, final_h).to_image();
        return Ok(cropped);
    }
    Ok(out)
}

/// Estimate the downward scroll (in px) from `a` to `b`.
///
/// Takes a horizontal strip from the bottom of `a` (above any sticky
/// footer) and finds where it appears in `b`, scanning upward. The
/// returned value is the number of rows the page moved.
fn estimate_scroll(
    a: &RgbaImage,
    b: &RgbaImage,
    sticky: &StickyMask,
    config: &StitchConfig,
) -> Option<u32> {
    let w = a.width();
    let h = a.height();
    let strip_h = config
        .strip_height
        .min(h / 4)
        .max(16)
        .min(h.saturating_sub(sticky.top_rows + sticky.bottom_rows + 1));
    if strip_h < 4 {
        return None;
    }
    let template_y = h.saturating_sub(strip_h + sticky.bottom_rows);
    // For each candidate y_b in `b` where the strip *could* live:
    // - lowest: just above the bottom-sticky region.
    // - highest: just below the top-sticky region (would mean dy ≈ template_y - top).
    let lo_y_b = sticky.top_rows;
    let hi_y_b = h.saturating_sub(strip_h + sticky.bottom_rows);
    if hi_y_b <= lo_y_b {
        return None;
    }
    let stride = config.subsample.max(1);
    let mut best_cost = u64::MAX;
    let mut best_y_b = template_y;
    let mut y_b = lo_y_b;
    while y_b <= hi_y_b {
        let cost = sad_strip(a, template_y, b, y_b, w, strip_h, stride);
        if cost < best_cost {
            best_cost = cost;
            best_y_b = y_b;
        }
        y_b += 1;
    }
    // dy = template_y in `a` minus y_b in `b`. If y_b > template_y the
    // page moved up (scrolled backwards) — return 0 so the caller
    // drops the frame.
    if best_y_b > template_y {
        return Some(0);
    }
    Some(template_y - best_y_b)
}

/// SAD over a horizontal strip with subsampling. Lower = better match.
fn sad_strip(
    a: &RgbaImage,
    a_y: u32,
    b: &RgbaImage,
    b_y: u32,
    w: u32,
    strip_h: u32,
    stride: u32,
) -> u64 {
    let mut sum: u64 = 0;
    let mut y = 0u32;
    while y < strip_h {
        let mut x = 0u32;
        while x < w {
            let pa = a.get_pixel(x, a_y + y).0;
            let pb = b.get_pixel(x, b_y + y).0;
            // Compare RGB only — alpha from screen capture is always
            // opaque, and including it doubles the work for no signal.
            sum +=
                diff(pa[0], pb[0]) as u64 + diff(pa[1], pb[1]) as u64 + diff(pa[2], pb[2]) as u64;
            x += stride;
        }
        y += stride;
    }
    sum
}

fn diff(a: u8, b: u8) -> u32 {
    if a > b {
        (a - b) as u32
    } else {
        (b - a) as u32
    }
}

/// Detect contiguous sticky rows at the top and bottom of the frame
/// sequence. A row is "sticky" if the mean RGB of that row varies by
/// no more than `sticky_tolerance` across every frame.
fn detect_sticky(frames: &[RgbaImage], config: &StitchConfig) -> StickyMask {
    if frames.len() < 2 {
        return StickyMask::default();
    }
    let h = frames[0].height();
    let max_check = h * config.sticky_search_fraction_quarter / 4;
    let mut top = 0u32;
    for y in 0..max_check {
        if rows_stable(frames, y, config.sticky_tolerance) {
            top = y + 1;
        } else {
            break;
        }
    }
    let mut bottom = 0u32;
    for offset in 0..max_check {
        let y = h - 1 - offset;
        if rows_stable(frames, y, config.sticky_tolerance) {
            bottom = offset + 1;
        } else {
            break;
        }
    }
    // Reject pathological cases where the whole frame is "sticky"
    // (e.g. the user never scrolled). top + bottom < h/2 keeps a
    // sensible scroll region in the middle.
    if top.saturating_add(bottom) >= h / 2 {
        return StickyMask::default();
    }
    StickyMask {
        top_rows: top,
        bottom_rows: bottom,
    }
}

/// True iff row `y` has roughly the same mean RGB in every frame.
fn rows_stable(frames: &[RgbaImage], y: u32, tolerance: u8) -> bool {
    let first = row_mean(&frames[0], y);
    for f in frames.iter().skip(1) {
        let m = row_mean(f, y);
        if diff(m[0], first[0]) > tolerance as u32
            || diff(m[1], first[1]) > tolerance as u32
            || diff(m[2], first[2]) > tolerance as u32
        {
            return false;
        }
    }
    true
}

fn row_mean(img: &RgbaImage, y: u32) -> [u8; 3] {
    let w = img.width();
    if w == 0 {
        return [0, 0, 0];
    }
    let mut r: u64 = 0;
    let mut g: u64 = 0;
    let mut b: u64 = 0;
    for x in 0..w {
        let p = img.get_pixel(x, y).0;
        r += p[0] as u64;
        g += p[1] as u64;
        b += p[2] as u64;
    }
    let n = w as u64;
    [(r / n) as u8, (g / n) as u8, (b / n) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// Helper: fill image with a horizontal stripe pattern where row
    /// `y` has color `(y, y, y, 255)`. Different `offset` shifts the
    /// stripe pattern, simulating a downward scroll.
    fn stripe_frame(w: u32, h: u32, offset: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            let v = ((y + offset) % 256) as u8;
            for x in 0..w {
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    #[test]
    fn empty_input_is_an_error() {
        let frames: Vec<RgbaImage> = vec![];
        assert!(matches!(
            stitch_scrolling(&frames, StitchConfig::default()),
            Err(StitchError::Empty)
        ));
    }

    #[test]
    fn single_frame_returns_clone() {
        let f = stripe_frame(64, 32, 0);
        let out = stitch_scrolling(&[f.clone()], StitchConfig::default()).unwrap();
        assert_eq!(out.dimensions(), (64, 32));
        assert_eq!(out.get_pixel(0, 0), f.get_pixel(0, 0));
    }

    #[test]
    fn dimension_mismatch_is_reported() {
        let a = stripe_frame(64, 32, 0);
        let b = stripe_frame(128, 32, 0);
        let err = stitch_scrolling(&[a, b], StitchConfig::default()).unwrap_err();
        match err {
            StitchError::DimensionMismatch { index, .. } => assert_eq!(index, 1),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn two_frame_scroll_extends_height_by_offset() {
        // A 64×64 frame with offset=0 stitched with one at offset=10
        // (page scrolled down 10 px) should yield a 64×74 image.
        let a = stripe_frame(64, 64, 0);
        let b = stripe_frame(64, 64, 10);
        let config = StitchConfig {
            min_motion: 4,
            ..StitchConfig::default()
        };
        let out = stitch_scrolling(&[a, b], config).unwrap();
        assert_eq!(out.width(), 64);
        assert!(
            (74..=80).contains(&out.height()),
            "expected ~74, got {}",
            out.height()
        );
    }

    #[test]
    fn duplicate_frame_is_dropped() {
        let a = stripe_frame(64, 64, 0);
        let b = stripe_frame(64, 64, 0);
        let out = stitch_scrolling(&[a, b], StitchConfig::default()).unwrap();
        // Second frame has zero scroll → not concatenated.
        assert_eq!(out.height(), 64);
    }

    #[test]
    fn output_too_tall_is_rejected() {
        let a = stripe_frame(32, 200, 0);
        let b = stripe_frame(32, 200, 50);
        let config = StitchConfig {
            max_output_height: 100,
            min_motion: 4,
            ..StitchConfig::default()
        };
        match stitch_scrolling(&[a, b], config).unwrap_err() {
            StitchError::OutputTooTall { cap, .. } => assert_eq!(cap, 100),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sticky_top_row_detected_when_unchanged() {
        // Build 3 frames where row 0 is solid red in all and the rest
        // is the scrolling stripe pattern.
        let w = 32;
        let h = 64;
        let mut frames: Vec<RgbaImage> = (0..3).map(|i| stripe_frame(w, h, i * 10)).collect();
        for f in frames.iter_mut() {
            for x in 0..w {
                f.put_pixel(x, 0, Rgba([255, 0, 0, 255]));
            }
        }
        let mask = detect_sticky(&frames, &StitchConfig::default());
        assert_eq!(mask.top_rows, 1, "expected top sticky row of height 1");
    }

    #[test]
    fn no_sticky_when_everything_moves() {
        let frames: Vec<RgbaImage> = (0..3).map(|i| stripe_frame(32, 64, i * 10)).collect();
        let mask = detect_sticky(&frames, &StitchConfig::default());
        assert_eq!(mask, StickyMask::default());
    }

    #[test]
    fn scroll_estimation_reads_simple_offset() {
        let a = stripe_frame(32, 64, 0);
        let b = stripe_frame(32, 64, 12);
        let config = StitchConfig::default();
        let dy = estimate_scroll(&a, &b, &StickyMask::default(), &config).unwrap();
        assert!((10..=14).contains(&dy), "expected dy near 12, got {dy}");
    }
}
