//! Cairo + Pango drawing for the popup. Builds the row list once per frame,
//! lays it out top-down, then blits the surface to the X window via XPutImage.

use anyhow::{anyhow, Result};
use cairo::{Context, Format, ImageSurface};

use crate::config::{Config, Rgba};
use crate::ewmh::{DesktopInfo, WindowInfo};
use crate::icon::surface_from_icon;

const PAD_X: f64 = 16.0;
const APP_INDENT: f64 = 22.0;
const ICON_SIZE: f64 = 18.0;
/// Gap between icon and text in app rows.
const ICON_TEXT_GAP: f64 = 10.0;
/// Hard cap on the dim-title length before we ellipsize it.
const TITLE_MAX_CHARS: usize = 60;
/// Thickness of the green drop-target underline on a desktop header.
const DROP_UNDERLINE_H: f64 = 2.0;

/// Two-column cheatsheet shown in the F1 help overlay.
const BINDINGS: &[(&str, &str)] = &[
    ("j / k, ↓ / ↑", "move cursor"),
    ("gg / G", "jump to first / last row"),
    ("Enter", "switch to desktop / focus app"),
    ("1–9, 0", "jump to desktop 1–10"),
    ("Shift+J / Shift+K", "move app one desktop down / up"),
    ("Shift+Ctrl+J / Shift+Ctrl+K", "move app and follow"),
    ("Click", "switch to desktop / focus app"),
    ("Drag app onto desktop", "move app"),
    ("Shift+drag", "move app and follow"),
    ("dd", "close window"),
    ("F1", "toggle this help"),
    ("Esc / q / Super+D", "close popup"),
];

/// Owned row kind so `Layout` doesn't borrow the desktop/window slices.
#[derive(Clone, Copy, Debug)]
pub enum Row {
    Header { desktop_idx: usize, current: bool, empty: bool },
    App { window_idx: usize, focused: bool },
}

pub struct Layout {
    pub rows: Vec<Row>,
    pub row_y: Vec<f64>,
    pub row_h: Vec<f64>,
    pub content_h: f64,
}

impl Layout {
    pub fn build(
        desktops: &[DesktopInfo],
        windows: &[WindowInfo],
        current_desktop: u32,
        focused_window: Option<u32>,
        pad_y: f64,
        header_h: f64,
        app_h: f64,
    ) -> Self {
        let mut rows: Vec<Row> = Vec::new();
        let mut row_y = Vec::new();
        let mut row_h = Vec::new();
        let mut y = pad_y;
        for (di, d) in desktops.iter().enumerate() {
            // _NET_CLIENT_LIST_STACKING is bottom-to-top; reverse so the
            // most-recently-raised (focused) window is at the top of the list.
            let app_indices: Vec<usize> = windows
                .iter()
                .enumerate()
                .filter(|(_, w)| w.desktop == d.index || w.desktop == u32::MAX)
                .map(|(i, _)| i)
                .rev()
                .collect();
            rows.push(Row::Header {
                desktop_idx: di,
                current: d.index == current_desktop,
                empty: app_indices.is_empty(),
            });
            row_y.push(y);
            row_h.push(header_h);
            y += header_h;
            for wi in app_indices {
                rows.push(Row::App {
                    window_idx: wi,
                    focused: focused_window == Some(windows[wi].id),
                });
                row_y.push(y);
                row_h.push(app_h);
                y += app_h;
            }
        }
        y += pad_y;
        Self { rows, row_y, row_h, content_h: y }
    }

    pub fn row_at_y(&self, y_in_window: f64, scroll: f64) -> Option<usize> {
        let target = y_in_window + scroll;
        self.row_y
            .iter()
            .zip(&self.row_h)
            .position(|(&ry, &rh)| target >= ry && target < ry + rh)
    }
}

#[derive(Clone, Debug)]
pub struct DragOverlay {
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub label: String,
    pub target_desktop: Option<u32>,
}

pub struct Renderer {
    pub w: i32,
    pub h: i32,
    icons: std::collections::HashMap<u32, ImageSurface>,
}

impl Renderer {
    pub fn new(w: i32, h: i32) -> Self {
        Self { w, h, icons: Default::default() }
    }

    fn icon_for(&mut self, win: &WindowInfo) -> Option<&ImageSurface> {
        if !self.icons.contains_key(&win.id) {
            if let Some(raw) = &win.icon {
                if let Ok(surf) = surface_from_icon(raw, ICON_SIZE as u32 * 2) {
                    self.icons.insert(win.id, surf);
                }
            }
        }
        self.icons.get(&win.id)
    }

    pub fn draw(
        &mut self,
        cfg: &Config,
        layout: &Layout,
        desktops: &[DesktopInfo],
        windows: &[WindowInfo],
        cursor: usize,
        scroll: f64,
        drag: Option<&DragOverlay>,
    ) -> Result<Vec<u8>> {
        let (surface, ctx) = new_canvas(self.w, self.h)?;
        paint_chrome(&ctx, cfg, self.w as f64, self.h as f64);

        let w = self.w as f64;
        let h = self.h as f64;
        let inset_x = cfg.border_thickness;

        ctx.save().ok();
        ctx.rectangle(0.0, 0.0, w, h);
        ctx.clip();
        ctx.translate(0.0, -scroll);

        for (i, row) in layout.rows.iter().enumerate() {
            let y = layout.row_y[i];
            let row_h = layout.row_h[i];
            if y + row_h < scroll || y > scroll + h {
                continue;
            }

            if i == cursor {
                set_rgba(&ctx, cfg.theme.cursor);
                ctx.rectangle(inset_x, y, w - 2.0 * inset_x, row_h);
                let _ = ctx.fill();
            }

            if let (Some(d), Row::Header { desktop_idx, .. }) = (drag, row) {
                if Some(desktops[*desktop_idx].index) == d.target_desktop {
                    set_rgba(&ctx, cfg.theme.drop_target);
                    ctx.rectangle(
                        inset_x,
                        y + row_h - DROP_UNDERLINE_H,
                        w - 2.0 * inset_x,
                        DROP_UNDERLINE_H,
                    );
                    let _ = ctx.fill();
                }
            }

            match *row {
                Row::Header { desktop_idx, current, empty } => {
                    draw_header(&ctx, cfg, y, row_h, &desktops[desktop_idx], current, empty);
                }
                Row::App { window_idx, focused } => {
                    let icon = self.icon_for(&windows[window_idx]);
                    draw_app(&ctx, cfg, y, row_h, &windows[window_idx], focused, icon, w);
                }
            }
        }
        ctx.restore().ok();

        if let Some(d) = drag {
            draw_drag_ghost(&ctx, cfg, d, w);
        }

        finish(surface, ctx, self.w as usize, self.h as usize)
    }

    pub fn draw_help(&mut self, cfg: &Config) -> Result<Vec<u8>> {
        let (surface, ctx) = new_canvas(self.w, self.h)?;
        paint_chrome(&ctx, cfg, self.w as f64, self.h as f64);

        let w = self.w as f64;
        let h = self.h as f64;

        ctx.save().ok();
        ctx.rectangle(0.0, 0.0, w, h);
        ctx.clip();

        let mut y = cfg.border_thickness;
        // Title row.
        let title_layout = pango_layout(&ctx, cfg, "Key bindings", true);
        set_rgba(&ctx, cfg.theme.header_current);
        let baseline = y + (cfg.header_height - layout_height(&title_layout)) / 2.0;
        ctx.move_to(PAD_X, baseline);
        pangocairo::functions::show_layout(&ctx, &title_layout);
        y += cfg.header_height;

        // Two-column rows.
        let key_col_w = (w * 0.45).max(180.0);
        let desc_x = PAD_X + key_col_w;
        for (keys, desc) in BINDINGS {
            let row_h = cfg.app_height;

            let key_layout = pango_layout(&ctx, cfg, keys, cfg.bold_apps);
            ellipsize(&key_layout, key_col_w);
            set_rgba(&ctx, cfg.theme.header);
            let baseline = y + (row_h - layout_height(&key_layout)) / 2.0;
            ctx.move_to(PAD_X, baseline);
            pangocairo::functions::show_layout(&ctx, &key_layout);

            let desc_layout = pango_layout(&ctx, cfg, desc, false);
            ellipsize(&desc_layout, w - desc_x - PAD_X);
            set_rgba(&ctx, cfg.theme.app);
            ctx.move_to(desc_x, baseline);
            pangocairo::functions::show_layout(&ctx, &desc_layout);

            y += row_h;
        }
        ctx.restore().ok();

        finish(surface, ctx, self.w as usize, self.h as usize)
    }
}

pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// -- frame helpers -----------------------------------------------------------

fn new_canvas(w: i32, h: i32) -> Result<(ImageSurface, Context)> {
    let surface = ImageSurface::create(Format::ARgb32, w, h)
        .map_err(|e| anyhow!("ImageSurface::create: {e}"))?;
    let ctx = Context::new(&surface).map_err(|e| anyhow!("Context::new: {e}"))?;
    Ok((surface, ctx))
}

/// Background fill + optional 1px-ish border. Shared between the row view and
/// the help overlay so both stay visually in sync.
fn paint_chrome(ctx: &Context, cfg: &Config, w: f64, h: f64) {
    set_rgba(ctx, cfg.background_with_opacity());
    let _ = ctx.paint();

    if cfg.border_thickness > 0.0 {
        let lw = cfg.border_thickness;
        set_rgba(ctx, cfg.theme.border);
        ctx.set_line_width(lw);
        ctx.rectangle(lw / 2.0, lw / 2.0, w - lw, h - lw);
        let _ = ctx.stroke();
    }
}

fn finish(mut surface: ImageSurface, ctx: Context, w: usize, h: usize) -> Result<Vec<u8>> {
    // Cairo refuses to lend the raw buffer while any Context still references
    // the surface — drop ours before asking for pixels.
    drop(ctx);
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| anyhow!("surface data: {e}"))?;
    let row_bytes = w * 4;
    if stride == row_bytes {
        return Ok(data.to_vec());
    }
    let mut out = Vec::with_capacity(h * row_bytes);
    for row in 0..h {
        out.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
    }
    Ok(out)
}

// -- row painters ------------------------------------------------------------

fn draw_header(
    ctx: &Context,
    cfg: &Config,
    y: f64,
    h: f64,
    d: &DesktopInfo,
    current: bool,
    empty: bool,
) {
    let name = if d.name.is_empty() {
        format!("Desktop {}", d.index + 1)
    } else {
        d.name.clone()
    };
    let color = if current {
        cfg.theme.header_current
    } else if empty {
        cfg.theme.header_empty
    } else {
        cfg.theme.header
    };
    set_rgba(ctx, color);

    let layout = pango_layout(ctx, cfg, &name, cfg.bold_headers);
    let text_top = y + (h - layout_height(&layout)) / 2.0;

    // Marker circle is centered against the same logical text box we use to
    // place the header label, which keeps it visually centered regardless of
    // the specific glyph ink in a desktop name.
    let cy = text_top + layout_height(&layout) / 2.0;
    let radius = (cfg.font_size * 0.24).max(3.0);
    let cx = PAD_X + radius;
    ctx.new_path();
    ctx.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
    if current {
        let _ = ctx.fill();
    } else {
        ctx.set_line_width(1.2);
        let _ = ctx.stroke();
    }

    let text_x = PAD_X + radius * 2.0 + 8.0;
    ctx.move_to(text_x, text_top);
    pangocairo::functions::show_layout(ctx, &layout);
}

fn draw_app(
    ctx: &Context,
    cfg: &Config,
    y: f64,
    h: f64,
    w: &WindowInfo,
    focused: bool,
    icon: Option<&ImageSurface>,
    win_w: f64,
) {
    let ix = PAD_X + APP_INDENT;
    let iy = y + (h - ICON_SIZE) / 2.0;
    draw_icon(ctx, icon, ix, iy);

    let class_src = if w.class.is_empty() { w.title.as_str() } else { w.class.as_str() };
    let class_display = capitalize(class_src);
    let dim_title = (!w.class.is_empty() && !w.title.is_empty() && w.title != w.class)
        .then(|| truncate(&w.title, TITLE_MAX_CHARS));

    let primary_color = if focused { cfg.theme.app_focused.hex() } else { cfg.theme.app.hex() };
    let name_span = format!(
        "<span foreground=\"{}\">{}</span>",
        primary_color,
        escape_markup(&class_display)
    );
    let mut markup = if cfg.bold_apps { format!("<b>{}</b>", name_span) } else { name_span };
    if let Some(t) = dim_title {
        markup.push_str(&format!(
            "  <span foreground=\"{}\">{}</span>",
            cfg.theme.app_dim.hex(),
            escape_markup(&t)
        ));
    }

    let text_x = ix + ICON_SIZE + ICON_TEXT_GAP;
    let layout = pangocairo::functions::create_layout(ctx);
    layout.set_font_description(Some(&make_font(cfg, false)));
    layout.set_markup(&markup);
    ellipsize(&layout, win_w - text_x - PAD_X);
    let baseline = y + (h - layout_height(&layout)) / 2.0;
    ctx.move_to(text_x, baseline);
    pangocairo::functions::show_layout(ctx, &layout);
}

fn draw_icon(ctx: &Context, icon: Option<&ImageSurface>, ix: f64, iy: f64) {
    if let Some(s) = icon {
        let sw = s.width() as f64;
        let sh = s.height() as f64;
        let scale = ICON_SIZE / sw.max(sh);
        ctx.save().ok();
        ctx.translate(ix, iy);
        ctx.scale(scale, scale);
        let _ = ctx.set_source_surface(s, 0.0, 0.0);
        let _ = ctx.paint();
        ctx.restore().ok();
    } else {
        ctx.set_source_rgba(1.0, 1.0, 1.0, 0.12);
        ctx.rectangle(ix, iy, ICON_SIZE, ICON_SIZE);
        let _ = ctx.fill();
    }
}

fn draw_drag_ghost(ctx: &Context, cfg: &Config, d: &DragOverlay, win_w: f64) {
    let x = d.cursor_x as f64 + 8.0;
    let y = d.cursor_y as f64 + 4.0;
    let w = 220.0_f64.min(win_w - x - 8.0);
    if w < 60.0 {
        return;
    }
    let h = cfg.app_height;
    ctx.set_source_rgba(0.12, 0.16, 0.22, 0.92);
    ctx.rectangle(x, y, w, h);
    let _ = ctx.fill();
    set_rgba(ctx, cfg.theme.drop_target);
    ctx.set_line_width(1.0);
    ctx.rectangle(x + 0.5, y + 0.5, w - 1.0, h - 1.0);
    let _ = ctx.stroke();

    let layout = pango_layout(ctx, cfg, &d.label, false);
    ellipsize(&layout, w - 12.0);
    set_rgba(ctx, cfg.theme.header_current);
    let baseline = y + (h - layout_height(&layout)) / 2.0;
    ctx.move_to(x + 8.0, baseline);
    pangocairo::functions::show_layout(ctx, &layout);
}

// -- pango / cairo helpers ---------------------------------------------------

fn set_rgba(ctx: &Context, c: Rgba) {
    ctx.set_source_rgba(c.r, c.g, c.b, c.a);
}

fn make_font(cfg: &Config, bold: bool) -> pango::FontDescription {
    let mut desc = pango::FontDescription::from_string(&cfg.font_family);
    if bold {
        desc.set_weight(pango::Weight::Bold);
    }
    desc.set_absolute_size(cfg.font_size * pango::SCALE as f64);
    desc
}

fn pango_layout(ctx: &Context, cfg: &Config, text: &str, bold: bool) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(ctx);
    layout.set_font_description(Some(&make_font(cfg, bold)));
    layout.set_text(text);
    layout
}

fn ellipsize(layout: &pango::Layout, avail_px: f64) {
    layout.set_width((avail_px * pango::SCALE as f64) as i32);
    layout.set_ellipsize(pango::EllipsizeMode::End);
}

fn layout_height(layout: &pango::Layout) -> f64 {
    let (_w, h) = layout.pixel_size();
    h as f64
}

fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}
