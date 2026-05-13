//! Pick the best size out of an `_NET_WM_ICON` payload and turn it into a
//! cairo ARGB32 surface so the renderer can paint it directly.

use anyhow::{anyhow, Result};
use cairo::{Format, ImageSurface};

/// Layout of one image inside the icon array: w, h, then w*h pixels.
struct IconView<'a> {
    w: u32,
    h: u32,
    pixels: &'a [u32],
}

fn iter_icons(raw: &[u32]) -> impl Iterator<Item = IconView<'_>> {
    let mut rest = raw;
    std::iter::from_fn(move || {
        if rest.len() < 2 {
            return None;
        }
        let w = rest[0];
        let h = rest[1];
        let n = (w as usize).checked_mul(h as usize)?;
        if w == 0 || h == 0 || rest.len() < 2 + n {
            return None;
        }
        let pixels = &rest[2..2 + n];
        rest = &rest[2 + n..];
        Some(IconView { w, h, pixels })
    })
}

/// Choose the icon whose larger side is closest to (and preferably >=) `target`.
fn pick_best(raw: &[u32], target: u32) -> Option<IconView<'_>> {
    let mut best: Option<IconView<'_>> = None;
    let mut best_score = i64::MAX;
    for ic in iter_icons(raw) {
        let big = ic.w.max(ic.h) as i64;
        let t = target as i64;
        // Prefer >= target: penalize undersized icons more than oversized.
        let score = if big >= t { big - t } else { (t - big) * 2 };
        if score < best_score {
            best_score = score;
            best = Some(ic);
        }
    }
    best
}

/// Turn an `_NET_WM_ICON` payload into a cairo surface (premultiplied ARGB32).
pub fn surface_from_icon(raw: &[u32], target_px: u32) -> Result<ImageSurface> {
    let ic = pick_best(raw, target_px).ok_or_else(|| anyhow!("no usable icon"))?;
    let w = ic.w as i32;
    let h = ic.h as i32;
    let mut surface = ImageSurface::create(Format::ARgb32, w, h)?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().map_err(|e| anyhow!("surface data: {e}"))?;
        for row in 0..ic.h as usize {
            let src = &ic.pixels[row * ic.w as usize..(row + 1) * ic.w as usize];
            let dst = &mut data[row * stride..row * stride + ic.w as usize * 4];
            for (i, px) in src.iter().enumerate() {
                let a = ((*px >> 24) & 0xff) as u32;
                let r = ((*px >> 16) & 0xff) as u32;
                let g = ((*px >> 8) & 0xff) as u32;
                let b = (*px & 0xff) as u32;
                // Cairo expects premultiplied alpha.
                let pr = (r * a / 255) as u8;
                let pg = (g * a / 255) as u8;
                let pb = (b * a / 255) as u8;
                // ARGB32 in memory on little-endian is B, G, R, A.
                let off = i * 4;
                dst[off] = pb;
                dst[off + 1] = pg;
                dst[off + 2] = pr;
                dst[off + 3] = a as u8;
            }
        }
    }
    Ok(surface)
}
