//! Per-profile window icon: the profile's own name on a platform-shaped badge,
//! so a dozen windows are tellable apart in the Dock, taskbar and Alt-Tab.
//! Composed and cached here; the browser only loads the file.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::ImageDecoder;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

const FONT: &[u8] = include_bytes!("../assets/NotoSans-Bold.ttf");
/// The product mark, drawn into the accent band so a profile icon still reads
/// as ShardX at a glance.
const MARK: &[u8] = include_bytes!("../icons/icon.png");

/// Rendered at 512 and downscaled by the toolkit.
const SIZE: f32 = 512.0;

/// Per-platform geometry, matching what each shell draws around an icon.
struct Geometry {
    /// Inset from the canvas edge, as a fraction of SIZE.
    margin: f32,
    /// Corner radius as a fraction of SIZE (ignored for the squircle).
    radius: f32,
    squircle: bool,
    shadow: bool,
    outline: bool,
}

const fn geometry() -> Geometry {
    #[cfg(target_os = "macos")]
    {
        Geometry { margin: 0.098, radius: 0.0, squircle: true, shadow: true, outline: false }
    }
    #[cfg(target_os = "windows")]
    {
        Geometry { margin: 0.045, radius: 0.115, squircle: false, shadow: false, outline: false }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Geometry { margin: 0.055, radius: 0.165, squircle: false, shadow: false, outline: true }
    }
}

/// Superellipse exponent; 5.0 is close to the macOS curve.
const N: f32 = 5.0;

/// Accent for a profile with no colour of its own: derived from the name, so
/// the same profile is always the same colour. It is the only thing still
/// telling profiles apart at 32 px, where no type is legible.
fn accent(name: &str) -> Color {
    let mut h: u64 = 1469598103934665603;
    for b in name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let hue = (h % 360) as f32;
    let (r, g, b) = hsv_to_rgb(hue, 0.70, 0.86);
    Color::from_rgba8(r, g, b, 255)
}

/// `#rgb` / `#rrggbb`, as the colour picker writes it.
fn parse_hex(raw: &str) -> Option<Color> {
    let h = raw.trim().trim_start_matches('#');
    let (r, g, b) = match h.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1].repeat(2), 16).ok();
            (d(0)?, d(1)?, d(2)?)
        }
        6 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            (d(0)?, d(2)?, d(4)?)
        }
        _ => return None,
    };
    Some(Color::from_rgba8(r, g, b, 255))
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h as u32) / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

fn squircle(cx: f32, cy: f32, a: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let steps = 720;
    for i in 0..steps {
        let t = std::f32::consts::TAU * (i as f32) / (steps as f32);
        let (st, ct) = t.sin_cos();
        let x = cx + a * ct.abs().powf(2.0 / N) * ct.signum();
        let y = cy + a * st.abs().powf(2.0 / N) * st.signum();
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.close();
    pb.finish()
}

/// Rounded rectangle; tiny-skia has none. Quadratic corners are under a pixel
/// off at 512 px.
fn rounded_rect(l: f32, t: f32, r: f32, b: f32, rad: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(r - rad, t);
    pb.quad_to(r, t, r, t + rad);
    pb.line_to(r, b - rad);
    pb.quad_to(r, b, r - rad, b);
    pb.line_to(l + rad, b);
    pb.quad_to(l, b, l, b - rad);
    pb.line_to(l, t + rad);
    pb.quad_to(l, t, l + rad, t);
    pb.close();
    pb.finish()
}

/// The body outline for this platform, plus its bounding box.
fn body_path(g: &Geometry) -> Option<(tiny_skia::Path, (f32, f32, f32, f32))> {
    let m = SIZE * g.margin;
    let (l, t, r, b) = (m, m, SIZE - m, SIZE - m);
    let path = if g.squircle {
        squircle(SIZE / 2.0, SIZE / 2.0, (r - l) / 2.0)?
    } else {
        rounded_rect(l, t, r, b, SIZE * g.radius)?
    };
    Some((path, (l, t, r, b)))
}

/// Wraps `name` to at most `max_lines` and picks the largest size that fits.
fn layout<'a>(
    font: &FontRef<'a>,
    name: &str,
    max_w: f32,
    max_h: f32,
    max_lines: usize,
) -> (f32, Vec<String>) {
    let words: Vec<&str> = name.split_whitespace().collect();
    let words = if words.is_empty() { vec![name] } else { words };

    let mut px = SIZE * 0.21;
    while px > 14.0 {
        let scaled = font.as_scaled(PxScale::from(px));
        let width = |s: &str| -> f32 {
            s.chars()
                .map(|c| scaled.h_advance(scaled.scaled_glyph(c).id))
                .sum()
        };
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        for w in &words {
            let cand = if cur.is_empty() {
                w.to_string()
            } else {
                format!("{cur} {w}")
            };
            if width(&cand) <= max_w {
                cur = cand;
            } else {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                cur = w.to_string();
                // A word wider than the line is ellipsised, not overflowed.
                while !cur.is_empty() && width(&format!("{cur}…")) > max_w {
                    cur.pop();
                }
                if width(w) > max_w {
                    cur.push('…');
                }
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        if lines.len() <= max_lines && (lines.len() as f32) * px * 1.16 <= max_h {
            return (px, lines);
        }
        px -= 4.0;
    }
    (14.0, vec![name.chars().take(9).collect()])
}

fn draw_text(
    pix: &mut Pixmap,
    font: &FontRef<'_>,
    lines: &[String],
    px: f32,
    cx: f32,
    top: f32,
    color: (u8, u8, u8),
) {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut y = top + scaled.ascent();
    for line in lines {
        let w: f32 = line
            .chars()
            .map(|c| scaled.h_advance(scaled.scaled_glyph(c).id))
            .sum();
        let mut x = cx - w / 2.0;
        for ch in line.chars() {
            let glyph = scaled.scaled_glyph(ch);
            let advance = scaled.h_advance(glyph.id);
            if let Some(outline) = font.outline_glyph(glyph.clone()) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    if cov <= 0.0 {
                        return;
                    }
                    let px_x = (bounds.min.x + gx as f32 + x) as i32;
                    let px_y = (bounds.min.y + gy as f32 + y) as i32;
                    if px_x < 0 || px_y < 0 {
                        return;
                    }
                    let (w, h) = (pix.width() as i32, pix.height() as i32);
                    if px_x >= w || px_y >= h {
                        return;
                    }
                    // tiny_skia has no glyph rasteriser: ab_glyph gives coverage,
                    // we composite it.
                    let idx = (px_y * w + px_x) as usize * 4;
                    let data = pix.data_mut();
                    let a = cov.clamp(0.0, 1.0);
                    for (i, c) in [color.0, color.1, color.2].iter().enumerate() {
                        let dst = data[idx + i] as f32;
                        data[idx + i] = (dst * (1.0 - a) + (*c as f32) * a) as u8;
                    }
                    let dst_a = data[idx + 3] as f32;
                    data[idx + 3] = (dst_a * (1.0 - a) + 255.0 * a) as u8;
                });
            }
            x += advance;
        }
        y += px * 1.16;
    }
}

const MAX_UPLOAD: usize = 10 * 1024 * 1024;
const MAX_ENCODED: usize = 2 * 1024 * 1024;

/// Import once, rather than persisting an external path that can disappear.
pub fn import_avatar(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).context("open image")?;
    let mut bytes = Vec::new();
    file.take(MAX_UPLOAD as u64 + 1).read_to_end(&mut bytes)?;
    normalize_avatar(&bytes)
}

fn normalize_avatar(bytes: &[u8]) -> Result<String> {
    anyhow::ensure!(bytes.len() <= MAX_UPLOAD, "Choose an image smaller than 10 MB");
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    anyhow::ensure!(matches!(reader.format(), Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP)),
        "Choose a PNG, JPEG or WebP image");
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().context("read image")?;
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder).context("decode image")?;
    image.apply_orientation(orientation);
    let rgba = image.resize_to_fill(SIZE as u32, SIZE as u32, image::imageops::FilterType::Lanczos3).to_rgba8();
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba).write_to(&mut png, image::ImageFormat::Png)?;
    Ok(STANDARD.encode(png.into_inner()))
}

/// Validate persisted input before allocating. Only our normalized RGBA PNGs
/// are accepted, including when the profile arrives through import or the API.
pub(crate) fn decode_avatar(encoded: &str) -> Result<Pixmap> {
    anyhow::ensure!(encoded.len() <= MAX_ENCODED, "Profile icon is too large");
    let bytes = STANDARD.decode(encoded).context("invalid profile icon encoding")?;
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().context("invalid profile icon PNG")?;
    let info = reader.info();
    anyhow::ensure!(info.width == SIZE as u32 && info.height == SIZE as u32
        && info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
        "Profile icon must be a normalized 512 × 512 RGBA PNG");
    let mut rgba = vec![0; reader.output_buffer_size()];
    reader.next_frame(&mut rgba).context("decode profile icon PNG")?;
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(SIZE as u32, SIZE as u32).unwrap())
        .context("allocate profile icon")
}

/// Custom icons are keyed by their content, so rename/colour changes keep them.
/// Standard icons retain the original cache key and rendering.
pub fn ensure_icon(cache_dir: &Path, name: &str, color: Option<&str>, icon_png: Option<&str>) -> Result<PathBuf> {
    let avatar = icon_png.map(decode_avatar).transpose()?;
    let key = format!("{name}\u{1}{}", color.unwrap_or(""));
    let mut h: u64 = 1469598103934665603;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let dir = cache_dir.join("profile-icons");
    std::fs::create_dir_all(&dir).ok();
    let filename = match icon_png {
        Some(encoded) => format!("avatar-v1-{:x}.png", Sha256::digest(encoded.as_bytes())),
        None => format!("{h:016x}.png"),
    };
    let out = dir.join(filename);
    if out.exists() {
        return Ok(out);
    }

    let g = geometry();
    let s = SIZE as u32;
    let mut pix = Pixmap::new(s, s).context("allocate pixmap")?;
    let (body, (x0, y0, x1, y1)) = body_path(&g).context("body path")?;
    let cx = (x0 + x1) / 2.0;

    let mut paint = Paint::default();
    paint.anti_alias = true;

    // Shadow (macOS only): stacked offset copies at low alpha — tiny-skia has
    // no blur, and at icon sizes the stack is indistinguishable.
    if g.shadow {
        for (i, alpha) in [(3.0_f32, 16u8), (6.0, 14), (10.0, 11), (15.0, 8)] {
            if let Some(sh) = body_path(&g).map(|(p, _)| p) {
                paint.set_color(Color::from_rgba8(0, 0, 0, alpha));
                let t = Transform::from_translate(0.0, i)
                    .pre_scale(1.0 + i / SIZE, 1.0 + i / SIZE)
                    .pre_translate(-cx * (i / SIZE), -(y0 + y1) / 2.0 * (i / SIZE));
                pix.fill_path(&sh, &paint, FillRule::Winding, t, None);
            }
        }
    }

    // White body.
    paint.set_color(Color::from_rgba8(255, 255, 255, 255));
    pix.fill_path(&body, &paint, FillRule::Winding, Transform::identity(), None);

    if let Some(avatar) = avatar {
        let mut clip = tiny_skia::Mask::new(s, s).context("mask")?;
        clip.fill_path(&body, FillRule::Winding, true, Transform::identity());
        let scale = (x1 - x0) / SIZE;
        pix.draw_pixmap(0, 0, avatar.as_ref(), &tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bicubic,
            ..Default::default()
        }, Transform::from_scale(scale, scale).post_translate(x0, y0), Some(&clip));
        if g.outline {
            paint.set_color(Color::from_rgba8(0, 0, 0, 38));
            pix.stroke_path(&body, &paint, &tiny_skia::Stroke { width: SIZE * 0.006, ..Default::default() }, Transform::identity(), None);
        }
        // Publish atomically: preview and launch can ask for the same image.
        let temp = out.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        pix.save_png(&temp).context("write avatar PNG")?;
        std::fs::rename(&temp, &out).context("cache avatar PNG")?;
        return Ok(out);
    }

    // Accent band, clipped to the body so its corners follow the same curve.
    let band_h = y0 + (y1 - y0) * 0.30;
    let mut band = PathBuilder::new();
    band.push_rect(tiny_skia::Rect::from_ltrb(0.0, 0.0, SIZE, band_h).context("band rect")?);
    if let Some(rect) = band.finish() {
        let mut clip = tiny_skia::Mask::new(s, s).context("mask")?;
        clip.fill_path(&body, FillRule::Winding, true, Transform::identity());
        paint.set_color(color.and_then(parse_hex).unwrap_or_else(|| accent(name)));
        pix.fill_path(&rect, &paint, FillRule::Winding, Transform::identity(), Some(&clip));
        draw_mark(&mut pix, cx, (y0 + band_h) / 2.0, (band_h - y0) * 0.62);
    }

    // Hairline outline, Linux only.
    if g.outline {
        paint.set_color(Color::from_rgba8(0, 0, 0, 38));
        let stroke = tiny_skia::Stroke { width: SIZE * 0.006, ..Default::default() };
        pix.stroke_path(&body, &paint, &stroke, Transform::identity(), None);
    }

    let font = FontRef::try_from_slice(FONT).context("load font")?;
    let top = y0 + (y1 - y0) * 0.36;
    let (px, lines) = layout(&font, name, (x1 - x0) * 0.82, (y1 - top) * 0.72, 3);
    let block_h = lines.len() as f32 * px * 1.16;
    let text_top = top + ((y1 - top) - block_h) / 2.0;
    draw_text(&mut pix, &font, &lines, px, cx, text_top, (34, 36, 42));

    pix.save_png(&out).context("write icon png")?;
    Ok(out)
}

/// The product mark, centred on (`cx`, `cy`) at `height` pixels.
fn draw_mark(pix: &mut Pixmap, cx: f32, cy: f32, height: f32) {
    let Some(mark) = mark_pixmap() else { return };
    if height <= 1.0 {
        return;
    }
    let scale = height / mark.height() as f32;
    let w = mark.width() as f32 * scale;
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bicubic,
        ..Default::default()
    };
    // draw_pixmap places the pixmap's origin at (x, y) and then applies the
    // transform, so the scale has to be folded into the offset by hand.
    pix.draw_pixmap(
        0,
        0,
        mark.as_ref(),
        &paint,
        Transform::from_scale(scale, scale)
            .post_translate(cx - w / 2.0, cy - height / 2.0),
        None,
    );
}

/// The mark, decoded once and premultiplied — tiny-skia wants premultiplied
/// alpha, PNG stores straight.
fn mark_pixmap() -> Option<&'static Pixmap> {
    static CACHE: std::sync::OnceLock<Option<Pixmap>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut reader = png::Decoder::new(MARK).read_info().ok()?;
            let mut buf = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).ok()?;
            if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
                return None;
            }
            buf.truncate(info.buffer_size());
            for px in buf.chunks_exact_mut(4) {
                let a = px[3] as u32;
                for c in 0..3 {
                    px[c] = ((px[c] as u32 * a + 127) / 255) as u8;
                }
            }
            Pixmap::from_vec(buf, tiny_skia::IntSize::from_wh(info.width, info.height)?)
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(format: image::ImageFormat) -> Vec<u8> {
        // Only the middle square is green; a stretch or off-centre crop shows red.
        let pixels = image::RgbImage::from_fn(600, 300, |x, _| {
            if (150..450).contains(&x) { image::Rgb([20, 180, 80]) }
            else { image::Rgb([230, 20, 30]) }
        });
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(pixels).write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn photos_are_center_cropped_and_normalized_for_all_supported_formats() {
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::WebP] {
            let encoded = normalize_avatar(&photo(format)).unwrap();
            let pix = decode_avatar(&encoded).unwrap();
            assert_eq!((pix.width(), pix.height()), (512, 512));
            for x in [20, 256, 490] {
                let pixel = pix.pixel(x, 256).unwrap();
                assert!(pixel.green() > 150 && pixel.red() < 50);
                assert_eq!(pixel.alpha(), 255);
            }
        }
    }

    #[test]
    fn corrupt_oversized_and_non_normalized_images_are_rejected() {
        assert!(normalize_avatar(b"not an image").is_err());
        assert!(normalize_avatar(&vec![0; MAX_UPLOAD + 1]).is_err());
        assert!(decode_avatar("not base64").is_err());
        assert!(decode_avatar(&"A".repeat(MAX_ENCODED + 1)).is_err());
        assert!(decode_avatar(&STANDARD.encode(photo(image::ImageFormat::Png))).is_err());
        let oversized = image::RgbImage::new(8193, 1);
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(oversized).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        assert!(normalize_avatar(bytes.get_ref()).is_err());
    }

    #[test]
    fn avatar_cache_survives_rename_color_change_and_source_removal() {
        let dir = std::env::temp_dir().join(format!("shardx-avatar-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("photo.jpg");
        std::fs::write(&source, photo(image::ImageFormat::Jpeg)).unwrap();
        let encoded = import_avatar(&source).unwrap();
        std::fs::remove_file(&source).unwrap();
        let a = ensure_icon(&dir, "Example profile", None, Some(&encoded)).unwrap();
        let b = ensure_icon(&dir, "New name", Some("#ff0000"), Some(&encoded)).unwrap();
        assert_eq!(a, b);
        let icon = Pixmap::load_png(&a).unwrap();
        assert_eq!(icon.pixel(0, 0).unwrap().alpha(), 0);
        let corner = (SIZE * geometry().margin) as u32 + 1;
        assert_eq!(icon.pixel(corner, corner).unwrap().alpha(), 0);
        assert!(icon.pixel(256, 256).unwrap().green() > 150);
        // Full image at the top as well: no leftover accent band or lettering.
        assert!(icon.pixel(256, 80).unwrap().green() > 150);
        let standard = ensure_icon(&dir, "Example profile", None, None).unwrap();
        assert_ne!(a, standard);
        assert_eq!(Pixmap::load_png(standard).unwrap().pixel(256, 400).unwrap().red(), 255);
        // Embedded metadata survives copy/export/import without an external path.
        let stored: crate::profile::StoredProfile = serde_json::from_value(serde_json::json!({
            "name": "Example profile", "_meta": {"icon_png": encoded}
        })).unwrap();
        let copy: crate::profile::StoredProfile = serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
        assert_eq!(stored.meta.icon_png, copy.meta.icon_png);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

