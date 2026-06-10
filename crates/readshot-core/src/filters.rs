//! Per-pixel filters used by `Annotation::Blur` and `Annotation::Pixelate`.
//!
//! Both operate **in place** on a `tiny_skia::Pixmap`'s premultiplied RGBA
//! buffer and are restricted to a rectangular region. Operating in
//! pixmap-space (rather than building intermediate `image::RgbaImage`
//! buffers) keeps the renderer's draw loop allocation-free per annotation.
//!
//! ## Blur
//!
//! A three-pass box blur, which is a fast and visually-acceptable
//! Gaussian approximation. Three passes of a box of radius `r` produce a
//! curve indistinguishable from a Gaussian of σ ≈ r/√3 for everyday
//! photographic content. The implementation bounds-clamps reads so the
//! filter is safe even when the rect touches the pixmap edge.
//!
//! ## Pixelate
//!
//! Subdivide the rect into `block_size × block_size` cells, average the
//! colour of every pixel inside each cell, and replace every pixel in the
//! cell with that average. This is *secure* pixelation in the Flameshot
//! sense — the result is a single colour per block with no original
//! sub-block detail leaking through.

use tiny_skia::Pixmap;

/// In-place three-pass box blur restricted to the rectangle
/// `(rect_x, rect_y, rect_w, rect_h)`. The rectangle is clipped to the
/// pixmap's bounds; supplying a rect that lies entirely outside the
/// pixmap is a no-op.
pub fn blur_rect(
    pixmap: &mut Pixmap,
    rect_x: i32,
    rect_y: i32,
    rect_w: i32,
    rect_h: i32,
    radius: f32,
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let x0 = rect_x.max(0);
    let y0 = rect_y.max(0);
    let x1 = (rect_x + rect_w).min(pw);
    let y1 = (rect_y + rect_h).min(ph);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let r = radius.round().max(1.0) as i32;

    // Three passes of horizontal-then-vertical box blur.
    for _ in 0..3 {
        box_blur_horizontal(pixmap, x0, y0, x1, y1, r);
        box_blur_vertical(pixmap, x0, y0, x1, y1, r);
    }
}

// Both passes use a sliding-window running sum, so each pass is
// O(width × height) regardless of radius: stepping the window drops
// the element leaving on the left and adds the one entering on the
// right. Reads are edge-clamped to the rect, identical to the previous
// per-pixel implementation (the window always holds 2r+1 clamped
// samples), just without re-summing the whole window per pixel.

fn box_blur_horizontal(pixmap: &mut Pixmap, x0: i32, y0: i32, x1: i32, y1: i32, r: i32) {
    let pw = pixmap.width() as i32;
    let data = pixmap.data_mut();
    let mut row = vec![0u8; ((x1 - x0) as usize) * 4];
    let count = (2 * r + 1) as u32;
    for y in y0..y1 {
        let base = (y * pw) as usize * 4;
        let mut acc = [0u32; 4];
        for dx in -r..=r {
            let sx = (x0 + dx).clamp(x0, x1 - 1);
            let i = base + sx as usize * 4;
            acc[0] += data[i] as u32;
            acc[1] += data[i + 1] as u32;
            acc[2] += data[i + 2] as u32;
            acc[3] += data[i + 3] as u32;
        }
        for x in x0..x1 {
            let dst = ((x - x0) as usize) * 4;
            row[dst] = (acc[0] / count) as u8;
            row[dst + 1] = (acc[1] / count) as u8;
            row[dst + 2] = (acc[2] / count) as u8;
            row[dst + 3] = (acc[3] / count) as u8;
            let drop_i = base + (x - r).clamp(x0, x1 - 1) as usize * 4;
            let add_i = base + (x + r + 1).clamp(x0, x1 - 1) as usize * 4;
            for c in 0..4 {
                acc[c] = acc[c] + data[add_i + c] as u32 - data[drop_i + c] as u32;
            }
        }
        for x in x0..x1 {
            let dst = (y * pw + x) as usize * 4;
            let src = ((x - x0) as usize) * 4;
            data[dst] = row[src];
            data[dst + 1] = row[src + 1];
            data[dst + 2] = row[src + 2];
            data[dst + 3] = row[src + 3];
        }
    }
}

fn box_blur_vertical(pixmap: &mut Pixmap, x0: i32, y0: i32, x1: i32, y1: i32, r: i32) {
    let pw = pixmap.width() as i32;
    let data = pixmap.data_mut();
    let mut col = vec![0u8; ((y1 - y0) as usize) * 4];
    let count = (2 * r + 1) as u32;
    for x in x0..x1 {
        let mut acc = [0u32; 4];
        for dy in -r..=r {
            let sy = (y0 + dy).clamp(y0, y1 - 1);
            let i = (sy * pw + x) as usize * 4;
            acc[0] += data[i] as u32;
            acc[1] += data[i + 1] as u32;
            acc[2] += data[i + 2] as u32;
            acc[3] += data[i + 3] as u32;
        }
        for y in y0..y1 {
            let dst = ((y - y0) as usize) * 4;
            col[dst] = (acc[0] / count) as u8;
            col[dst + 1] = (acc[1] / count) as u8;
            col[dst + 2] = (acc[2] / count) as u8;
            col[dst + 3] = (acc[3] / count) as u8;
            let drop_i = ((y - r).clamp(y0, y1 - 1) * pw + x) as usize * 4;
            let add_i = ((y + r + 1).clamp(y0, y1 - 1) * pw + x) as usize * 4;
            for c in 0..4 {
                acc[c] = acc[c] + data[add_i + c] as u32 - data[drop_i + c] as u32;
            }
        }
        for y in y0..y1 {
            let dst = (y * pw + x) as usize * 4;
            let src = ((y - y0) as usize) * 4;
            data[dst] = col[src];
            data[dst + 1] = col[src + 1];
            data[dst + 2] = col[src + 2];
            data[dst + 3] = col[src + 3];
        }
    }
}

/// Mean of the four channels in `acc` divided by `count`. Returns `None`
/// when `count == 0` so the caller can `if let Some(avg) = ...` without a
/// manual `> 0` guard (clippy's `manual_checked_division` warns on the
/// guard).
fn checked_average(acc: [u32; 4], count: u32) -> Option<[u8; 4]> {
    if count == 0 {
        return None;
    }
    Some([
        (acc[0] / count) as u8,
        (acc[1] / count) as u8,
        (acc[2] / count) as u8,
        (acc[3] / count) as u8,
    ])
}

/// In-place pixelate: replace every pixel in each `block × block` cell
/// inside the rect with the cell's average colour. Cells that overlap the
/// rect's edge are partially-clipped — the average is taken over only the
/// pixels actually inside the rect.
pub fn pixelate_rect(
    pixmap: &mut Pixmap,
    rect_x: i32,
    rect_y: i32,
    rect_w: i32,
    rect_h: i32,
    block: f32,
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let x0 = rect_x.max(0);
    let y0 = rect_y.max(0);
    let x1 = (rect_x + rect_w).min(pw);
    let y1 = (rect_y + rect_h).min(ph);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let block = block.round().max(2.0) as i32;
    let data = pixmap.data_mut();

    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            let cell_x_end = (bx + block).min(x1);
            let cell_y_end = (by + block).min(y1);
            // Compute the cell's average premultiplied RGBA.
            let mut acc = [0u32; 4];
            let mut count = 0u32;
            for y in by..cell_y_end {
                for x in bx..cell_x_end {
                    let i = (y * pw + x) as usize * 4;
                    acc[0] += data[i] as u32;
                    acc[1] += data[i + 1] as u32;
                    acc[2] += data[i + 2] as u32;
                    acc[3] += data[i + 3] as u32;
                    count += 1;
                }
            }
            if let Some(avg) = checked_average(acc, count) {
                for y in by..cell_y_end {
                    for x in bx..cell_x_end {
                        let i = (y * pw + x) as usize * 4;
                        data[i] = avg[0];
                        data[i + 1] = avg[1];
                        data[i + 2] = avg[2];
                        data[i + 3] = avg[3];
                    }
                }
            }
            bx += block;
        }
        by += block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_pixmap(w: u32, h: u32, rgba: [u8; 4]) -> Pixmap {
        let mut p = Pixmap::new(w, h).unwrap();
        let data = p.data_mut();
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        p
    }

    #[test]
    fn pixelate_collapses_block_to_average() {
        // 4×4 pixmap, half white half black, 2×2 pixelate cells.
        let mut p = Pixmap::new(4, 4).unwrap();
        let data = p.data_mut();
        for y in 0..4 {
            for x in 0..4 {
                let i = (y * 4 + x) * 4;
                let v: u8 = if x < 2 { 255 } else { 0 };
                data[i] = v;
                data[i + 1] = v;
                data[i + 2] = v;
                data[i + 3] = 255;
            }
        }
        pixelate_rect(&mut p, 0, 0, 4, 4, 2.0);
        // Each 2×2 block was uniform inside; results should equal the
        // block's input value (255 for left block, 0 for right block).
        let data = p.data();
        // (0, 0) — left block — should be 255.
        assert_eq!(data[0], 255);
        // (2, 0) — right block — should be 0.
        assert_eq!(data[2 * 4], 0);
    }

    #[test]
    fn blur_no_op_on_uniform_region() {
        // Blurring a flat colour leaves it flat. (Within u8 rounding.)
        let mut p = solid_pixmap(8, 8, [128, 64, 32, 255]);
        blur_rect(&mut p, 0, 0, 8, 8, 2.0);
        let data = p.data();
        for px in data.chunks_exact(4) {
            assert_eq!(px, &[128, 64, 32, 255]);
        }
    }

    #[test]
    fn pixelate_with_partial_edge_cell() {
        // A 5-wide row pixelated with block=3 produces one full cell (0..3)
        // and one partial cell (3..5). Both should have well-defined
        // averages — no panics, no out-of-bounds.
        let mut p = solid_pixmap(5, 1, [10, 20, 30, 255]);
        pixelate_rect(&mut p, 0, 0, 5, 1, 3.0);
        let data = p.data();
        for px in data.chunks_exact(4) {
            assert_eq!(px, &[10, 20, 30, 255]);
        }
    }
}
