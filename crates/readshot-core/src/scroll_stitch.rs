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
//! - Assumes monotonically increasing scroll. A frame that went back
//!   up, moved sideways or can't be placed (no offset matches clearly;
//!   [`match_stands_out`]) is skipped, and the next frame is measured
//!   against the last frame that contributed rows.
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
    /// Number of overlapping rows to feather between the previous
    /// output and the next frame before appending newly revealed
    /// rows. This softens unavoidable sub-pixel / antialiasing
    /// differences at frame boundaries.
    pub seam_blend_rows: u32,
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
            seam_blend_rows: 8,
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

    // Each frame is placed against the last frame that was placed (the
    // anchor), and its scroll position is the anchor's plus the offset.
    // A frame adds the rows below the furthest frame pasted so far. A
    // frame that can't be placed (a scroll back up, a sideways move, a
    // change no offset explains) is skipped and leaves the anchor where
    // it is, so the next frame that does place carries on from there:
    // nothing is mis-stitched and one bad frame never stops the rest.
    let max_motion = config.max_motion.min(h.saturating_sub(1));
    let mut accepted: Vec<u32> = Vec::with_capacity(frames.len() - 1);
    let mut anchor = 0usize;
    let mut anchor_pos: u64 = 0;
    let mut furthest: u64 = 0;
    for (i, frame) in frames.iter().enumerate().skip(1) {
        let placed =
            place_frame(&frames[anchor], frame, &sticky, &config).filter(|&dy| dy <= max_motion);
        let mut dy = 0;
        if let Some(m) = placed {
            anchor = i;
            anchor_pos += u64::from(m);
            let new_rows = anchor_pos.saturating_sub(furthest);
            if new_rows >= u64::from(config.min_motion) {
                dy = u32::try_from(new_rows).unwrap_or(u32::MAX);
                furthest = anchor_pos;
            }
        }
        accepted.push(dy);
    }
    let extra: u32 = accepted.iter().sum();
    let total_h = h.saturating_add(extra);
    if total_h > config.max_output_height {
        return Err(StitchError::OutputTooTall {
            height: total_h,
            cap: config.max_output_height,
        });
    }

    // The sticky footer must render exactly once, at the very end of
    // the stitched output. Trim it off the base frame, exclude it from
    // every pasted strip, and re-append it after the loop. When no
    // frame contributes (no accepted motion) the base frame is kept
    // whole, footer in its natural place.
    let has_motion = accepted.iter().any(|&dy| dy > 0);
    let bottom = if has_motion { sticky.bottom_rows } else { 0 };

    // Paste the first frame at the top, minus the sticky footer.
    let mut out = RgbaImage::new(w, total_h);
    let base_h = h - bottom;
    let base = image::imageops::crop_imm(&frames[0], 0, 0, w, base_h).to_image();
    image::imageops::overlay(&mut out, &base, 0, 0);
    let mut y_cursor = base_h as i64;
    let mut last_contributing = 0usize;

    // For each accepted frame, paste the `dy` rows of newly revealed
    // content. New content lives just above the sticky footer: rows
    // `[h - bottom - dy .. h - bottom)`.
    for (i, &dy) in accepted.iter().enumerate() {
        if dy == 0 {
            continue;
        }
        let frame = &frames[i + 1];
        // Clamp to the scroll region so a large dy can't reach into
        // the sticky header.
        let src_y = (h - bottom).saturating_sub(dy).max(sticky.top_rows);
        let strip_h = (h - bottom).saturating_sub(src_y);
        if strip_h == 0 {
            continue;
        }
        // Source rows `[src_y - overlap .. src_y)` should overlap the
        // output's tail. Feather those rows first so tiny
        // anti-aliasing / fractional-scroll differences don't become
        // visible horizontal boundary lines. Then append the truly
        // new rows `[src_y .. h - bottom)`.
        let overlap_h = config
            .seam_blend_rows
            .min(src_y.saturating_sub(sticky.top_rows))
            .min(y_cursor.max(0) as u32);
        if overlap_h > 0 {
            blend_overlap_rows(
                &mut out,
                frame,
                OverlapBlend {
                    src_y: src_y - overlap_h,
                    dst_y: (y_cursor as u32).saturating_sub(overlap_h),
                    width: w,
                    height: overlap_h,
                },
            );
        }
        let strip = image::imageops::crop_imm(frame, 0, src_y, w, strip_h).to_image();
        image::imageops::overlay(&mut out, &strip, 0, y_cursor);
        y_cursor += strip_h as i64;
        last_contributing = i + 1;
    }

    // Re-append the sticky footer once, from the last contributing frame.
    if bottom > 0 {
        let footer =
            image::imageops::crop_imm(&frames[last_contributing], 0, h - bottom, w, bottom)
                .to_image();
        image::imageops::overlay(&mut out, &footer, 0, y_cursor);
        y_cursor += bottom as i64;
    }

    // If any strip was clamped, the canvas is taller than the pasted
    // content — clip.
    let final_h = (y_cursor.max(0) as u32).min(total_h);
    if final_h < total_h {
        let cropped = image::imageops::crop_imm(&out, 0, 0, w, final_h).to_image();
        return Ok(cropped);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct OverlapBlend {
    src_y: u32,
    dst_y: u32,
    width: u32,
    height: u32,
}

fn blend_overlap_rows(out: &mut RgbaImage, frame: &RgbaImage, blend: OverlapBlend) {
    for y in 0..blend.height {
        let next_weight = y + 1;
        let prev_weight = blend.height + 1 - next_weight;
        let denom = blend.height + 1;
        for x in 0..blend.width {
            let prev = out.get_pixel(x, blend.dst_y + y).0;
            let next = frame.get_pixel(x, blend.src_y + y).0;
            let blended = [
                blend_channel(prev[0], next[0], prev_weight, next_weight, denom),
                blend_channel(prev[1], next[1], prev_weight, next_weight, denom),
                blend_channel(prev[2], next[2], prev_weight, next_weight, denom),
                blend_channel(prev[3], next[3], prev_weight, next_weight, denom),
            ];
            out.put_pixel(x, blend.dst_y + y, image::Rgba(blended));
        }
    }
}

fn blend_channel(prev: u8, next: u8, prev_weight: u32, next_weight: u32, denom: u32) -> u8 {
    let total = prev as u32 * prev_weight + next as u32 * next_weight;
    ((total + denom / 2) / denom) as u8
}

/// Place `frame` below `prev`: the downward scroll between them, or
/// `None` when it can't be placed (no offset stands out) or the page
/// went back up. A backward reading wins only when it is the better
/// match: strictly cheaper, or as cheap and the shorter move (a page that
/// repeats matches both ways). An exact forward match always stands.
fn place_frame(
    prev: &RgbaImage,
    frame: &RgbaImage,
    sticky: &StickyMask,
    config: &StitchConfig,
) -> Option<u32> {
    place_match(prev, frame, sticky, config).map(|m| m.dy)
}

/// [`place_frame`], keeping the forward match's cost and exactness.
fn place_match(
    prev: &RgbaImage,
    frame: &RgbaImage,
    sticky: &StickyMask,
    config: &StitchConfig,
) -> Option<Match> {
    let fwd = estimate_scroll(prev, frame, sticky, config)?;
    let refused = estimate_scroll(frame, prev, sticky, config).is_some_and(|back| {
        back.dy >= config.min_motion
            && (back.cost < fwd.cost || (back.cost == fwd.cost && back.dy < fwd.dy))
    });
    (!refused).then_some(fwd)
}

/// The stitcher's own placement of `b` below `a` (bottom strip, the
/// whole frame searched, the backward check), for two frames with no
/// sticky rows known. The live scroll judge falls back on it when its
/// own template can't place a frame, so the judge accepts every move
/// the stitcher will place: anything up to the frame height minus the
/// strip. `None` for a frame that can't be placed or went back up;
/// `Some(dy)` may be below `config.min_motion` (a still page).
pub fn place_by_overlap(a: &RgbaImage, b: &RgbaImage, config: &StitchConfig) -> Option<u32> {
    if a.dimensions() != b.dimensions() || a.width() == 0 || a.height() == 0 {
        return None;
    }
    place_frame(a, b, &StickyMask::default(), config)
}

/// [`place_by_overlap`], but only for a near-exact match: the rows of
/// `a` really are in `b` at that offset (as on any real scroll), not a
/// line-aligned look-alike that merely stands out. The scroll judge
/// uses it to call a frame a scroll back up with confidence.
pub fn place_exactly_by_overlap(
    a: &RgbaImage,
    b: &RgbaImage,
    config: &StitchConfig,
) -> Option<u32> {
    if a.dimensions() != b.dimensions() || a.width() == 0 || a.height() == 0 {
        return None;
    }
    place_match(a, b, &StickyMask::default(), config)
        .filter(|m| m.exact)
        .map(|m| m.dy)
}

/// How many times better than the typical candidate a template match
/// must be before it is trusted as the scroll offset. A real scroll
/// matches (almost) exactly while every other offset doesn't; a page
/// that moved sideways, went back up past the search window, or changed
/// outright has no offset that stands out, and guessing one would
/// stitch in rows the page doesn't have.
pub const MATCH_DISTINCT_RATIO: u64 = 3;

/// Mean per-channel difference (0-255) under which a match is close
/// enough to trust whatever the other offsets cost: anti-aliasing and
/// sub-pixel jitter between two captures of the same rows.
pub const MATCH_EXACT_MEAN_DIFF: u64 = 2;

/// True when the best template-match cost `best` (a sum of absolute
/// channel differences over `samples` channel values) is trustworthy:
/// near-exact, or at least [`MATCH_DISTINCT_RATIO`] times lower than the
/// median cost of every candidate offset in `costs`. Shared by the
/// stitcher and the scroll-capture judge so both trust the same matches.
pub fn match_stands_out(best: u64, samples: u64, costs: &mut [u64]) -> bool {
    if costs.is_empty() {
        return false;
    }
    if best <= samples.saturating_mul(MATCH_EXACT_MEAN_DIFF) {
        return true;
    }
    let mid = costs.len() / 2;
    let (_, median, _) = costs.select_nth_unstable(mid);
    best.saturating_mul(MATCH_DISTINCT_RATIO) <= *median
}

/// A placed template match: the downward scroll and its (subsampled)
/// cost, comparable between the two directions of one frame pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Match {
    dy: u32,
    cost: u64,
    /// Near-exact: mean difference within [`MATCH_EXACT_MEAN_DIFF`].
    exact: bool,
}

/// Estimate the downward scroll (in px) from `a` to `b`.
///
/// Takes a horizontal strip from the bottom of `a` (above any sticky
/// footer) and finds where it appears in `b`, scanning upward. The
/// returned offset is the number of rows the page moved. `None` when no
/// offset matches clearly better than the rest ([`match_stands_out`]):
/// the frame can't be placed and is skipped.
fn estimate_scroll(
    a: &RgbaImage,
    b: &RgbaImage,
    sticky: &StickyMask,
    config: &StitchConfig,
) -> Option<Match> {
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
    let mut costs = Vec::with_capacity((hi_y_b - lo_y_b + 1) as usize);
    let mut best_cost = u64::MAX;
    let mut best_y_b = template_y;
    // Scan from no motion upward: equal-cost ties keep the smaller
    // motion, so blank or repeated content does not look like a scroll.
    let mut y_b = hi_y_b;
    loop {
        let cost = sad_strip(a, template_y, b, y_b, w, strip_h, stride);
        costs.push(cost);
        if cost < best_cost {
            best_cost = cost;
            best_y_b = y_b;
        }
        if y_b == lo_y_b {
            break;
        }
        y_b -= 1;
    }
    let samples = u64::from(w.div_ceil(stride)) * u64::from(strip_h.div_ceil(stride)) * 3;
    // `costs[k]` is the cost at `y_b = hi_y_b - k`; read before the
    // median reorders it.
    let coarse = |y: u32| costs[(hi_y_b - y) as usize];
    let tied: Vec<u32> = [best_y_b.wrapping_add(1), best_y_b.wrapping_sub(1)]
        .into_iter()
        .filter(|&y| (lo_y_b..=hi_y_b).contains(&y) && coarse(y) == best_cost)
        .collect();
    if !match_stands_out(best_cost, samples, &mut costs) {
        return None;
    }
    let y_b = break_subsampling_tie(best_y_b, &tied, |y| {
        sad_strip(a, template_y, b, y, w, strip_h, 1)
    });
    Some(Match {
        dy: template_y - y_b,
        cost: best_cost,
        exact: best_cost <= samples.saturating_mul(MATCH_EXACT_MEAN_DIFF),
    })
}

/// Subsampling skips rows, so the true offset and a neighbour can cost
/// exactly the same, and taking the wrong one leaves a 1 px seam. Among
/// `best` and its `tied` neighbours, keep the one that matches best at
/// full resolution; full ties keep the smaller motion (the larger `y`).
/// A neighbour that already cost more is never reconsidered, so one row
/// that really differs (anti-aliasing at the seam) can't pull the match
/// off by a pixel.
pub fn break_subsampling_tie(best: u32, tied: &[u32], cost: impl Fn(u32) -> u64) -> u32 {
    if tied.is_empty() {
        return best;
    }
    let mut pick = best;
    let mut pick_cost = cost(best);
    for &y in tied {
        let c = cost(y);
        if c < pick_cost || (c == pick_cost && y > pick) {
            pick = y;
            pick_cost = c;
        }
    }
    pick
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
    // Row slices, not `get_pixel`: this is the hot loop of every match.
    let row_bytes = w as usize * 4;
    let (ra, rb) = (a.as_raw(), b.as_raw());
    let (aw, bw) = (a.width() as usize * 4, b.width() as usize * 4);
    let step = stride as usize * 4;
    let mut sum: u64 = 0;
    let mut y = 0u32;
    while y < strip_h {
        let ia = (a_y + y) as usize * aw;
        let ib = (b_y + y) as usize * bw;
        let row_a = &ra[ia..ia + row_bytes];
        let row_b = &rb[ib..ib + row_bytes];
        let mut i = 0;
        let mut row_sum: u32 = 0;
        while i < row_bytes {
            // Compare RGB only — alpha from screen capture is always
            // opaque, and including it doubles the work for no signal.
            row_sum += u32::from(row_a[i].abs_diff(row_b[i]))
                + u32::from(row_a[i + 1].abs_diff(row_b[i + 1]))
                + u32::from(row_a[i + 2].abs_diff(row_b[i + 2]));
            i += step;
        }
        sum += u64::from(row_sum);
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
    // Clamp to `h` so an oversized `sticky_search_fraction_quarter`
    // can't push the bottom-scan offset past the frame.
    let max_check = (h * config.sticky_search_fraction_quarter / 4).min(h);
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
        let out = stitch_scrolling(std::slice::from_ref(&f), StitchConfig::default()).unwrap();
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

    /// SC-16: a frame that went back up is skipped, and the next frame
    /// is measured against the last frame that contributed rows, so the
    /// stitch is the page with no band repeated or lost.
    #[test]
    fn a_scroll_back_up_is_skipped_and_the_next_frame_joins_the_last_kept_one() {
        let config = StitchConfig {
            min_motion: 4,
            ..StitchConfig::default()
        };
        let frames = [0, 12, 0].map(|o| stripe_frame(32, 64, o));
        let out = stitch_scrolling(&frames, config).unwrap();
        assert_eq!(out, stripe_frame(32, 76, 0));
        let frames = [0, 12, 4, 24].map(|o| stripe_frame(32, 64, o));
        let out = stitch_scrolling(&frames, config).unwrap();
        assert_eq!(out, stripe_frame(32, 88, 0));
    }

    /// On a page that repeats every 40 rows, 12 px down and 28 px back
    /// up look the same downward; the shorter move wins, so a real 12 px
    /// scroll joins and a 12 px scroll back up is skipped.
    #[test]
    fn on_a_repeating_page_the_shorter_move_wins() {
        let period = |o: u32| {
            RgbaImage::from_fn(32, 64, |x, y| {
                let v = ((y + o) % 40 * 6) as u8;
                Rgba([v, v.wrapping_add(x as u8), 255 - v, 255])
            })
        };
        let config = StitchConfig {
            min_motion: 4,
            ..StitchConfig::default()
        };
        let out = stitch_scrolling(&[period(0), period(12)], config).unwrap();
        assert_eq!(out.height(), 76);
        let out = stitch_scrolling(&[period(0), period(12), period(0)], config).unwrap();
        assert_eq!(out.height(), 76, "the scroll back up is skipped");
    }

    /// A frame no offset explains (the content changed outright) is
    /// skipped rather than stitched at the cheapest wrong offset.
    #[test]
    fn a_frame_that_matches_nowhere_is_skipped() {
        let a = stripe_frame(32, 64, 0);
        let b = RgbaImage::from_fn(32, 64, |x, y| {
            let v = ((x * 97 + y * 31) ^ (x * y)) as u8;
            Rgba([v, v.wrapping_mul(7), v.wrapping_add(90), 255])
        });
        assert!(
            estimate_scroll(&a, &b, &StickyMask::default(), &StitchConfig::default()).is_none()
        );
        let out = stitch_scrolling(&[a.clone(), b], StitchConfig::default()).unwrap();
        assert_eq!(out, a);
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
        let dy = estimate_scroll(&a, &b, &StickyMask::default(), &config)
            .unwrap()
            .dy;
        assert!((10..=14).contains(&dy), "expected dy near 12, got {dy}");
    }

    /// Stripe frame with a solid-blue sticky footer overlaid on the
    /// bottom `footer` rows (same in every frame, like a fixed bar).
    fn footer_frame(w: u32, h: u32, offset: u32, footer: u32) -> RgbaImage {
        let mut img = stripe_frame(w, h, offset);
        for y in h - footer..h {
            for x in 0..w {
                img.put_pixel(x, y, Rgba([0, 0, 255, 255]));
            }
        }
        img
    }

    #[test]
    fn sticky_footer_renders_once_at_bottom_without_dropping_content() {
        let w = 32;
        let h = 64;
        let footer = 6;
        let frames: Vec<RgbaImage> = (0..3).map(|i| footer_frame(w, h, i * 12, footer)).collect();
        let config = StitchConfig {
            min_motion: 4,
            ..StitchConfig::default()
        };
        let out = stitch_scrolling(&frames, config).unwrap();

        // Two accepted scrolls of 12 px each: 64 + 12 + 12.
        assert_eq!(out.height(), 88, "expected full content height");

        // The footer appears exactly once, as the bottom rows.
        let is_footer_row = |y: u32| (0..w).all(|x| out.get_pixel(x, y).0 == [0, 0, 255, 255]);
        for y in out.height() - footer..out.height() {
            assert!(is_footer_row(y), "row {y} should be the sticky footer");
        }
        for y in 0..out.height() - footer {
            assert!(!is_footer_row(y), "sticky footer duplicated at row {y}");
        }

        // Content is continuous: stitched row `y` carries stripe value
        // `y` (the stripe pattern aligns across seams, so feathering is
        // value-preserving here).
        for y in [0u32, 30, 57, 60, 70, 81] {
            let v = out.get_pixel(0, y).0[0];
            assert_eq!(v as u32, y % 256, "content discontinuity at row {y}");
        }
    }

    /// Stripe frame with a solid-red sticky header overlaid on the top
    /// `header` rows (same in every frame, like a fixed nav bar).
    fn header_frame(w: u32, h: u32, offset: u32, header: u32) -> RgbaImage {
        let mut img = stripe_frame(w, h, offset);
        for y in 0..header {
            for x in 0..w {
                img.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        img
    }

    #[test]
    fn sticky_top_header_is_not_re_pasted_below_the_scroll_region() {
        // Exercises the sticky-*header* path through the full stitcher:
        // detect_sticky reports `top_rows`, estimate clamps its search to
        // `lo_y_b = top_rows`, and the paste loop's
        // `src_y = (h - bottom).saturating_sub(dy).max(sticky.top_rows)`
        // (the header clamp at ~204) keeps a strip from reaching up into
        // the header. The header stays a single band at the very top and
        // the scroll region below stitches normally.
        let w = 32;
        let h = 64;
        let header = 6;
        let frames: Vec<RgbaImage> = (0..3).map(|i| header_frame(w, h, i * 12, header)).collect();
        let config = StitchConfig {
            min_motion: 4,
            ..StitchConfig::default()
        };

        // Sanity: the header is detected as a sticky top band.
        let mask = detect_sticky(&frames, &config);
        assert_eq!(mask.top_rows, header);
        assert_eq!(mask.bottom_rows, 0);

        let out = stitch_scrolling(&frames, config).unwrap();
        // 64 + two accepted 12px scrolls.
        assert_eq!(out.height(), 88);

        let is_header_row = |y: u32| (0..w).all(|x| out.get_pixel(x, y).0 == [255, 0, 0, 255]);
        // The header band renders exactly once, at the top.
        for y in 0..header {
            assert!(is_header_row(y), "row {y} should be the sticky header");
        }
        for y in header..out.height() {
            assert!(
                !is_header_row(y),
                "header duplicated into scroll region at row {y}"
            );
        }
    }

    #[test]
    fn stitch_feathers_shared_overlap_before_appending_new_rows() {
        let w = 32;
        let h = 96;
        let dy = 12;
        let a = stripe_frame(w, h, 0);
        let mut b = stripe_frame(w, h, dy);

        // This row is in the shared overlap immediately before the
        // newly revealed strip. Real captures can differ here because
        // text antialiasing/shadows settle differently while scrolling.
        let overlap_y_in_b = h - dy - 1;
        for x in 0..w {
            b.put_pixel(x, overlap_y_in_b, Rgba([250, 250, 250, 255]));
        }

        let config = StitchConfig {
            min_motion: 4,
            ..StitchConfig::default()
        };
        let out = stitch_scrolling(&[a.clone(), b], config).unwrap();

        let boundary_overlap_row = h - 1;
        let before = a.get_pixel(0, boundary_overlap_row).0[0];
        let after = out.get_pixel(0, boundary_overlap_row).0[0];

        assert_ne!(
            after, before,
            "last pre-boundary row should be feathered with the next frame overlap"
        );
        assert!(
            after > before && after < 250,
            "expected a blended row between {before} and 250, got {after}"
        );
    }
}
