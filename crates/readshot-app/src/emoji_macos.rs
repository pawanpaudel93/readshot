//! macOS emoji rasterizer for emoji stamps.
//!
//! Draws one emoji with CoreText (Apple Color Emoji, by font name) into
//! an RGBA bitmap context. Registered with
//! `readshot_core::stamp::set_emoji_rasterizer` in `main`, so core never
//! reads the ~188 MB emoji font file itself. Core caches the result
//! once per process.

use std::ffi::c_void;
use std::ptr;

use image::RgbaImage;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CTFontRef = *const c_void;
type CGContextRef = *mut c_void;
type CGColorSpaceRef = *mut c_void;
type CGGlyph = u16;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
const K_CG_BITMAP_BYTE_ORDER_32_BIG: u32 = 4 << 12;
const K_CT_FONT_ORIENTATION_DEFAULT: u32 = 0;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> CFStringRef;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextRelease(context: CGContextRef);
}

#[link(name = "CoreText", kind = "framework")]
extern "C" {
    fn CTFontCreateWithName(name: CFStringRef, size: f64, matrix: *const c_void) -> CTFontRef;
    fn CTFontGetGlyphsForCharacters(
        font: CTFontRef,
        characters: *const u16,
        glyphs: *mut CGGlyph,
        count: isize,
    ) -> bool;
    fn CTFontGetBoundingRectsForGlyphs(
        font: CTFontRef,
        orientation: u32,
        glyphs: *const CGGlyph,
        rects: *mut CGRect,
        count: isize,
    ) -> CGRect;
    fn CTFontDrawGlyphs(
        font: CTFontRef,
        glyphs: *const CGGlyph,
        positions: *const CGPoint,
        count: usize,
        context: CGContextRef,
    );
}

/// `px` × `px` straight-alpha RGBA bitmap of `emoji`, centred, or `None`
/// if the font or glyph is unavailable. Matches
/// `readshot_core::stamp::EmojiRasterizer`.
pub fn rasterize_emoji(emoji: &str, px: u32) -> Option<RgbaImage> {
    if px == 0 || px > 1024 || emoji.is_empty() {
        return None;
    }
    let units: Vec<u16> = emoji.encode_utf16().collect();
    let name = "AppleColorEmoji";
    // SAFETY: every Create/Copy result is null-checked and released
    // exactly once; buffers outlive the calls that borrow them.
    unsafe {
        let cf_name = CFStringCreateWithBytes(
            ptr::null(),
            name.as_ptr(),
            name.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
            0,
        );
        if cf_name.is_null() {
            return None;
        }
        // Slightly under px so the ink (which can exceed the em box) fits.
        let font = CTFontCreateWithName(cf_name, px as f64 * 0.8, ptr::null());
        CFRelease(cf_name);
        if font.is_null() {
            return None;
        }
        let mut glyphs: Vec<CGGlyph> = vec![0; units.len()];
        let mapped = CTFontGetGlyphsForCharacters(
            font,
            units.as_ptr(),
            glyphs.as_mut_ptr(),
            units.len() as isize,
        );
        // A surrogate pair maps to one glyph followed by 0.
        let glyph = glyphs.iter().copied().find(|g| *g != 0);
        let image = match (mapped, glyph) {
            (true, Some(g)) => draw_glyph(font, g, px as usize),
            _ => None,
        };
        CFRelease(font);
        image
    }
}

/// Draw `glyph` centred in a `side` × `side` RGBA bitmap.
///
/// # Safety
///
/// `font` must be a valid, live `CTFontRef`; the caller keeps ownership.
unsafe fn draw_glyph(font: CTFontRef, glyph: CGGlyph, side: usize) -> Option<RgbaImage> {
    // SAFETY (whole body): the colour space and context are null-checked
    // and each released exactly once on every path. `buf` is
    // `side * side * 4` bytes, matching the context's width, height and
    // `bytes_per_row`; the context only writes through it until
    // `CGContextRelease`, and `buf` is not touched by Rust before then.
    // `glyph`, `ink` and `at` are single values matching `count == 1`.
    let mut buf = vec![0u8; side * side * 4];
    let space = CGColorSpaceCreateDeviceRGB();
    if space.is_null() {
        return None;
    }
    let ctx = CGBitmapContextCreate(
        buf.as_mut_ptr().cast(),
        side,
        side,
        8,
        side * 4,
        space,
        K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | K_CG_BITMAP_BYTE_ORDER_32_BIG,
    );
    CGColorSpaceRelease(space);
    if ctx.is_null() {
        return None;
    }
    let mut ink = CGRect::default();
    CTFontGetBoundingRectsForGlyphs(font, K_CT_FONT_ORIENTATION_DEFAULT, &glyph, &mut ink, 1);
    // CG's origin is bottom-left; centre the ink box.
    let at = CGPoint {
        x: (side as f64 - ink.size.width) / 2.0 - ink.origin.x,
        y: (side as f64 - ink.size.height) / 2.0 - ink.origin.y,
    };
    CTFontDrawGlyphs(font, &glyph, &at, 1, ctx);
    CGContextRelease(ctx);
    // Bitmap-context memory is top row first. Un-premultiply to the
    // straight RGBA that `image` and core expect.
    for px in buf.as_chunks_mut::<4>().0 {
        let a = px[3] as u16;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u16 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(side as u32, side as u32, buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbs_up_rasterizes_to_visible_color_pixels() {
        let img = rasterize_emoji("👍", 64).expect("Apple Color Emoji ships with macOS");
        assert_eq!(img.dimensions(), (64, 64));
        let opaque: Vec<_> = img.pixels().filter(|p| p[3] > 200).collect();
        assert!(opaque.len() > 200, "only {} opaque pixels", opaque.len());
        // Color, not a monochrome silhouette.
        assert!(opaque.iter().any(|p| p[0].abs_diff(p[2]) > 40));
    }

    #[test]
    fn rejects_empty_input_and_absurd_sizes() {
        assert!(rasterize_emoji("", 64).is_none());
        assert!(rasterize_emoji("👍", 0).is_none());
        assert!(rasterize_emoji("👍", 4096).is_none());
    }
}
