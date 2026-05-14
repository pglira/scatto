//! Cairo + Pango drawing for the popup. Builds the row list once per frame,
//! lays it out top-down, then blits the surface to the X window via XPutImage.

use anyhow::{anyhow, Result};
use cairo::{Context, Format, ImageSurface};

use crate::config::{Config, Rgba};
use crate::ewmh::{DesktopInfo, WindowInfo};
use crate::icon::surface_from_icon;

pub const PAD_X: f64 = 16.0;
pub const APP_INDENT: f64 = 22.0;
pub const ICON_SIZE: f64 = 18.0;

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
        for (i, &ry) in self.row_y.iter().enumerate() {
            if target >= ry && target < ry + self.row_h[i] {
                return Some(i);
            }
        }
        None
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
    pub icons: std::collections::HashMap<u32, ImageSurface>,
}

impl Renderer {
    pub fn new(w: i32, h: i32) -> Self {
        Self { w, h, icons: Default::default() }
    }

    pub fn icon_for(&mut self, win: &WindowInfo) -> Option<&ImageSurface> {
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
        let mut surface = ImageSurface::create(Format::ARgb32, self.w, self.h)
            .map_err(|e| anyhow!("ImageSurface::create: {e}"))?;
        let ctx = Context::new(&surface).map_err(|e| anyhow!("Context::new: {e}"))?;

        set_rgba(&ctx, cfg.background_with_opacity());
        let _ = ctx.paint();

        if cfg.border_thickness > 0.0 {
            let lw = cfg.border_thickness;
            set_rgba(&ctx, cfg.theme.border);
            ctx.set_line_width(lw);
            ctx.rectangle(lw / 2.0, lw / 2.0, self.w as f64 - lw, self.h as f64 - lw);
            let _ = ctx.stroke();
        }

        ctx.save().ok();
        ctx.rectangle(0.0, 0.0, self.w as f64, self.h as f64);
        ctx.clip();
        ctx.translate(0.0, -scroll);

        for (i, row) in layout.rows.iter().enumerate() {
            let y = layout.row_y[i];
            let h = layout.row_h[i];

            if y + h < scroll || y > scroll + self.h as f64 {
                continue;
            }

            let inset_x = cfg.border_thickness;
            let is_cursor = i == cursor;
            if is_cursor {
                set_rgba(&ctx, cfg.theme.cursor);
                ctx.rectangle(inset_x, y, self.w as f64 - 2.0 * inset_x, h);
                let _ = ctx.fill();
            }

            if let Some(d) = drag {
                if let Row::Header { desktop_idx, .. } = row {
                    if Some(desktops[*desktop_idx].index) == d.target_desktop {
                        set_rgba(&ctx, cfg.theme.drop_target);
                        ctx.rectangle(inset_x, y + h - 2.0, self.w as f64 - 2.0 * inset_x, 2.0);
                        let _ = ctx.fill();
                    }
                }
            }

            match row {
                Row::Header { desktop_idx, current, empty } => {
                    self.draw_header(&ctx, cfg, y, h, &desktops[*desktop_idx], *current, *empty);
                }
                Row::App { window_idx, focused } => {
                    self.draw_app(&ctx, cfg, y, h, &windows[*window_idx], *focused);
                }
            }
        }
        ctx.restore().ok();

        if let Some(d) = drag {
            self.draw_drag_ghost(&ctx, cfg, d);
        }

        surface.flush();
        // Cairo refuses to lend the raw buffer while any Context still
        // references the surface — drop ours before asking for pixels.
        drop(ctx);
        let stride = surface.stride() as usize;
        let data = surface.data().map_err(|e| anyhow!("surface data: {e}"))?;
        let expected = self.w as usize * 4;
        if stride == expected {
            return Ok(data.to_vec());
        }
        let mut out = Vec::with_capacity(self.h as usize * expected);
        for row in 0..self.h as usize {
            out.extend_from_slice(&data[row * stride..row * stride + expected]);
        }
        Ok(out)
    }

    fn draw_header(
        &self,
        ctx: &Context,
        cfg: &Config,
        y: f64,
        h: f64,
        d: &DesktopInfo,
        current: bool,
        empty: bool,
    ) {
        let marker = if current { "●" } else { "○" };
        let name = if d.name.is_empty() {
            format!("Desktop {}", d.index + 1)
        } else {
            d.name.clone()
        };
        let text = format!("{}  {}", marker, name);

        let color = if current {
            cfg.theme.header_current
        } else if empty {
            cfg.theme.header_empty
        } else {
            cfg.theme.header
        };
        set_rgba(ctx, color);

        let layout = pangocairo::functions::create_layout(ctx);
        let desc = make_font(cfg, cfg.bold_headers);
        layout.set_font_description(Some(&desc));
        layout.set_text(&text);
        let baseline = y + (h - layout_height(&layout)) / 2.0;
        ctx.move_to(PAD_X, baseline);
        pangocairo::functions::show_layout(ctx, &layout);
    }

    fn draw_app(&mut self, ctx: &Context, cfg: &Config, y: f64, h: f64, w: &WindowInfo, focused: bool) {
        let icon = self.icon_for(w);
        let ix = PAD_X + APP_INDENT;
        let iy = y + (h - ICON_SIZE) / 2.0;
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

        let class = if w.class.is_empty() { w.title.as_str() } else { w.class.as_str() };
        let class_display = pretty_class(class);
        let dim_title = if !w.class.is_empty() && !w.title.is_empty() && w.title != w.class {
            Some(w.title.clone())
        } else {
            None
        };

        let text_x = ix + ICON_SIZE + 10.0;
        let layout = pangocairo::functions::create_layout(ctx);
        let desc = make_font(cfg, false);
        layout.set_font_description(Some(&desc));

        let primary_color = if focused {
            cfg.theme.app_focused.hex()
        } else {
            cfg.theme.app.hex()
        };
        let name_span = format!(
            "<span foreground=\"{}\">{}</span>",
            primary_color,
            escape_markup(&class_display)
        );
        let mut markup = if cfg.bold_apps {
            format!("<b>{}</b>", name_span)
        } else {
            name_span
        };
        if let Some(t) = dim_title {
            let t = truncate(&t, 60);
            markup.push_str(&format!(
                "  <span foreground=\"{}\">{}</span>",
                cfg.theme.app_dim.hex(),
                escape_markup(&t)
            ));
        }
        layout.set_markup(&markup);
        let avail = self.w as f64 - text_x - PAD_X;
        layout.set_width((avail * pango::SCALE as f64) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);

        let baseline = y + (h - layout_height(&layout)) / 2.0;
        ctx.move_to(text_x, baseline);
        pangocairo::functions::show_layout(ctx, &layout);
    }

    fn draw_drag_ghost(&mut self, ctx: &Context, cfg: &Config, d: &DragOverlay) {
        let x = d.cursor_x as f64 + 8.0;
        let y = d.cursor_y as f64 + 4.0;
        let w = 220.0_f64.min(self.w as f64 - x - 8.0);
        if w < 60.0 {
            return;
        }
        let app_h = cfg.app_height;
        ctx.set_source_rgba(0.12, 0.16, 0.22, 0.92);
        ctx.rectangle(x, y, w, app_h);
        let _ = ctx.fill();
        set_rgba(ctx, cfg.theme.drop_target);
        ctx.set_line_width(1.0);
        ctx.rectangle(x + 0.5, y + 0.5, w - 1.0, app_h - 1.0);
        let _ = ctx.stroke();

        let layout = pangocairo::functions::create_layout(ctx);
        let desc = make_font(cfg, false);
        layout.set_font_description(Some(&desc));
        layout.set_text(&d.label);
        layout.set_width(((w - 12.0) * pango::SCALE as f64) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        set_rgba(ctx, cfg.theme.header_current);
        let baseline = y + (app_h - layout_height(&layout)) / 2.0;
        ctx.move_to(x + 8.0, baseline);
        pangocairo::functions::show_layout(ctx, &layout);
    }
}

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

fn pretty_class(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
