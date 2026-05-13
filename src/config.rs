//! TOML config loaded from `$XDG_CONFIG_HOME/scatto/config.toml` (or
//! `~/.config/scatto/config.toml`). Missing keys fall back to the same
//! values the renderer used when nothing was configurable.

/// Commented TOML printed by `scatto print-config`. Keep the values in sync
/// with `Config::default` below — this is the same theme, written out in
/// hex so the user can edit it as text.
pub const DEFAULT_CONFIG: &str = r##"# scatto — popup config. All keys are optional. Save this at
# ~/.config/scatto/config.toml (or pipe `scatto print-config` into it),
# then reopen the popup; the file is read once per launch.

[window]
# Popup width in px.
width = 520
# Max popup height in px. Content scrolls if taller.
max_height = 640
# Background transparency. 0.0 = fully transparent, 1.0 = opaque.
# Visible transparency requires a compositor (e.g. picom).
opacity = 0.94

[font]
# Any Pango font family.
family = "Sans"
# Absolute font size in px.
size = 13.0
# Bold weight on desktop headers.
bold_headers = true

[colors]
# Hex strings: #RGB, #RGBA, #RRGGBB, or #RRGGBBAA. Alpha is honored
# everywhere except `background`, whose alpha comes from `window.opacity`
# (the dedicated transparency knob).
background     = "#1a1c24"
border         = "#ffffff14"
cursor         = "#4d8ce547"
drop_target    = "#4dd98cd9"
header_current = "#f2f2ff"
header_empty   = "#a6a6b38c"
header         = "#d9d9e6d9"
app            = "#e8e8ee"
app_focused    = "#ffd166"
app_dim        = "#9c9ca8"
"##;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    /// `#RRGGBB` form for pango markup (alpha is ignored — pango expects
    /// solid color hex strings in `<span foreground="...">`).
    pub fn hex(&self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            (self.r * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.g * 255.0).round().clamp(0.0, 255.0) as u8,
            (self.b * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }
}

pub struct Theme {
    pub background: Rgba,
    pub border: Rgba,
    pub cursor: Rgba,
    pub drop_target: Rgba,
    pub header_current: Rgba,
    pub header_empty: Rgba,
    pub header: Rgba,
    pub app: Rgba,
    pub app_focused: Rgba,
    pub app_dim: Rgba,
}

pub struct Config {
    pub width: u16,
    pub max_height: u16,
    /// Background opacity (0.0 = transparent, 1.0 = opaque). Applied on top
    /// of whatever alpha `colors.background` had.
    pub opacity: f64,
    pub font_family: String,
    pub font_size: f64,
    pub bold_headers: bool,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 520,
            max_height: 640,
            opacity: 0.94,
            font_family: "Sans".to_string(),
            font_size: 13.0,
            bold_headers: true,
            theme: Theme {
                background: rgba(0.10, 0.11, 0.14, 1.0),
                border: rgba(1.0, 1.0, 1.0, 0.08),
                cursor: rgba(0.30, 0.55, 0.90, 0.28),
                drop_target: rgba(0.30, 0.85, 0.55, 0.85),
                header_current: rgba(0.95, 0.95, 1.00, 1.0),
                header_empty: rgba(0.65, 0.65, 0.70, 0.55),
                header: rgba(0.85, 0.85, 0.90, 0.85),
                app: hex("#e8e8ee"),
                app_focused: hex("#ffd166"),
                app_dim: hex("#9c9ca8"),
            },
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let raw: Raw = toml::from_str(&text)
            .with_context(|| format!("parse {}", path.display()))?;
        let mut cfg = Self::default();
        if let Some(w) = raw.window {
            if let Some(v) = w.width { cfg.width = v; }
            if let Some(v) = w.max_height { cfg.max_height = v; }
            if let Some(v) = w.opacity {
                cfg.opacity = v.clamp(0.0, 1.0);
            }
        }
        if let Some(f) = raw.font {
            if let Some(v) = f.family { cfg.font_family = v; }
            if let Some(v) = f.size {
                if v > 0.0 {
                    cfg.font_size = v;
                }
            }
            if let Some(v) = f.bold_headers { cfg.bold_headers = v; }
        }
        if let Some(c) = raw.colors {
            macro_rules! apply {
                ($field:ident) => {
                    if let Some(s) = c.$field {
                        cfg.theme.$field = parse_color(&s)
                            .with_context(|| format!("colors.{}", stringify!($field)))?;
                    }
                };
            }
            apply!(background);
            apply!(border);
            apply!(cursor);
            apply!(drop_target);
            apply!(header_current);
            apply!(header_empty);
            apply!(header);
            apply!(app);
            apply!(app_focused);
            apply!(app_dim);
        }
        Ok(cfg)
    }

    /// Background alpha actually used at paint time — `opacity` overrides
    /// whatever the parsed `background` color had, since `opacity` is the
    /// dedicated transparency knob.
    pub fn background_with_opacity(&self) -> Rgba {
        Rgba { a: self.opacity, ..self.theme.background }
    }
}

pub fn config_path() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = xdg.or_else(|| home.map(|h| h.join(".config")))?;
    Some(base.join("scatto").join("config.toml"))
}

/// Write the default config to `config_path()`, creating the parent dir if
/// needed. Refuses to overwrite an existing file — the user can `rm` it or
/// use `print-config >` if they want a different destination.
pub fn write_default_config() -> Result<PathBuf> {
    let path = config_path().ok_or_else(|| anyhow!("can't resolve config path (no HOME?)"))?;
    if path.exists() {
        return Err(anyhow!(
            "config already exists at {} — remove it first or use `scatto print-config` to stdout",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(&path, DEFAULT_CONFIG)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[derive(Debug, Deserialize, Default)]
struct Raw {
    window: Option<RawWindow>,
    font: Option<RawFont>,
    colors: Option<RawColors>,
}

#[derive(Debug, Deserialize, Default)]
struct RawWindow {
    width: Option<u16>,
    max_height: Option<u16>,
    opacity: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct RawFont {
    family: Option<String>,
    size: Option<f64>,
    bold_headers: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct RawColors {
    background: Option<String>,
    border: Option<String>,
    cursor: Option<String>,
    drop_target: Option<String>,
    header_current: Option<String>,
    header_empty: Option<String>,
    header: Option<String>,
    app: Option<String>,
    app_focused: Option<String>,
    app_dim: Option<String>,
}

fn rgba(r: f64, g: f64, b: f64, a: f64) -> Rgba {
    Rgba { r, g, b, a }
}

fn hex(s: &str) -> Rgba {
    parse_color(s).expect("built-in hex parsed at compile-time-equivalent failed")
}

/// Accepts `#RGB`, `#RGBA`, `#RRGGBB`, or `#RRGGBBAA`.
fn parse_color(s: &str) -> Result<Rgba> {
    let s = s.trim();
    let hex = s
        .strip_prefix('#')
        .ok_or_else(|| anyhow!("color must start with '#': {s:?}"))?;
    let (r, g, b, a) = match hex.len() {
        3 => {
            let r = h1(&hex[0..1])?;
            let g = h1(&hex[1..2])?;
            let b = h1(&hex[2..3])?;
            (r, g, b, 255u8)
        }
        4 => {
            let r = h1(&hex[0..1])?;
            let g = h1(&hex[1..2])?;
            let b = h1(&hex[2..3])?;
            let a = h1(&hex[3..4])?;
            (r, g, b, a)
        }
        6 => {
            let r = h2(&hex[0..2])?;
            let g = h2(&hex[2..4])?;
            let b = h2(&hex[4..6])?;
            (r, g, b, 255u8)
        }
        8 => {
            let r = h2(&hex[0..2])?;
            let g = h2(&hex[2..4])?;
            let b = h2(&hex[4..6])?;
            let a = h2(&hex[6..8])?;
            (r, g, b, a)
        }
        _ => return Err(anyhow!("invalid hex color length: {s:?}")),
    };
    Ok(Rgba {
        r: r as f64 / 255.0,
        g: g as f64 / 255.0,
        b: b as f64 / 255.0,
        a: a as f64 / 255.0,
    })
}

fn h1(s: &str) -> Result<u8> {
    let v = u8::from_str_radix(s, 16).map_err(|_| anyhow!("bad hex nibble: {s:?}"))?;
    Ok(v * 17) // expand #abc → #aabbcc
}

fn h2(s: &str) -> Result<u8> {
    u8::from_str_radix(s, 16).map_err(|_| anyhow!("bad hex byte: {s:?}"))
}
