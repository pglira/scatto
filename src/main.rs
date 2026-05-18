//! Entry point. Without arguments: open the popup. With `switch-next`,
//! `switch-prev`, `move-next`, `move-prev`: do that action and exit (so users
//! can also bind Super+Shift+J/K / Super+Ctrl+J/K directly in their WM).

mod config;
mod ewmh;
mod icon;
mod keymap;
mod popup;
mod render;

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask, NotifyMode};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::config::Config;
use crate::ewmh::{Atoms, DesktopInfo, Ewmh, WindowInfo};
use crate::keymap::{Key, Keymap};
use crate::popup::Popup;
use crate::render::{capitalize, DragOverlay, Layout, Renderer, Row};

fn main() -> Result<()> {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "" | "popup" => run_popup(),
        "switch-next" => action(1, false),
        "switch-prev" => action(-1, false),
        "move-next" => action(1, true),
        "move-prev" => action(-1, true),
        "print-config" => {
            print!("{}", crate::config::DEFAULT_CONFIG);
            Ok(())
        }
        "init-config" => {
            let path = crate::config::write_default_config()?;
            println!("wrote {}", path.display());
            Ok(())
        }
        "-h" | "--help" => {
            println!(
                "scatto [popup|switch-next|switch-prev|move-next|move-prev|print-config|init-config]"
            );
            Ok(())
        }
        other => {
            eprintln!("unknown subcommand: {}", other);
            std::process::exit(2);
        }
    }
}

// -- direct actions (for WM keybindings without opening the popup) ------------

fn action(delta: i32, take_active: bool) -> Result<()> {
    let (conn, screen_num) = RustConnection::connect(None).context("X11 connect")?;
    let root = conn.setup().roots[screen_num].root;
    let atoms = Atoms::new(&conn)?;
    let ew = Ewmh::new(&conn, root, &atoms);
    let n = ew.number_of_desktops()?;
    if n == 0 {
        return Ok(());
    }
    let cur = ew.current_desktop()? as i32;
    let next = ((cur + delta).rem_euclid(n as i32)) as u32;
    if take_active {
        if let Some(active) = ew.active_window()? {
            ew.move_window_to_desktop(active, next)?;
        }
    }
    ew.switch_desktop(next, x11rb::CURRENT_TIME)?;
    Ok(())
}

// -- popup --------------------------------------------------------------------

struct State {
    cfg: Config,
    desktops: Vec<DesktopInfo>,
    windows: Vec<WindowInfo>,
    current_desktop: u32,
    focused_window: Option<u32>,
    cursor: usize,
    scroll: f64,
    viewport_h: f64,
    drag: Option<Drag>,
    /// True after the first `d` of vim-style `dd` (close window). Cleared by
    /// any non-`d` press; the second `d` closes the app at the cursor. `d`
    /// with Super held is the popup-toggle and bypasses this entirely.
    pending_dd: bool,
    /// True after the first `g` of vim-style `gg` (jump to top).
    pending_gg: bool,
    /// True while the F1 key-bindings overlay is shown.
    show_help: bool,
    /// Window id under the cursor at middle-button press, awaiting release.
    /// Cleared on release; if the release lands on the same window, that app
    /// is closed. Holding the press while moving the pointer to a different
    /// row cancels — same escape hatch as left-click activation.
    middle_press: Option<u32>,
}

#[derive(Clone)]
struct Drag {
    window_id: u32,
    label: String,
    cursor_x: i32,
    cursor_y: i32,
    target_desktop: Option<u32>,
    press_x: i32,
    press_y: i32,
    /// Latched true once the cursor has moved past a small threshold; below
    /// that we treat the gesture as a click instead.
    started: bool,
    /// Shift held at press → on drop, follow the app to its new desktop
    /// (switching the user there) and make it the active window. Latched at
    /// press so releasing shift mid-drag doesn't change the action.
    follow: bool,
}

impl Drag {
    fn moved_enough(&self) -> bool {
        let dx = self.cursor_x - self.press_x;
        let dy = self.cursor_y - self.press_y;
        dx * dx + dy * dy > 16
    }
}

impl State {
    /// Open a temporary X11 connection, snapshot the WM state, drop the
    /// connection. Done BEFORE opening the popup so `_NET_ACTIVE_WINDOW` still
    /// reflects the app the user had focused.
    fn snapshot(cfg: Config) -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None).context("X11 connect")?;
        let root = conn.setup().roots[screen_num].root;
        let atoms = Atoms::new(&conn)?;
        let ew = Ewmh::new(&conn, root, &atoms);
        Ok(Self {
            cfg,
            desktops: ew.desktops()?,
            windows: ew.windows()?,
            current_desktop: ew.current_desktop()?,
            focused_window: ew.active_window().ok().flatten(),
            cursor: 0,
            scroll: 0.0,
            viewport_h: 0.0,
            drag: None,
            pending_dd: false,
            pending_gg: false,
            show_help: false,
            middle_press: None,
        })
    }

    fn layout(&self) -> Layout {
        Layout::build(
            &self.desktops,
            &self.windows,
            self.current_desktop,
            self.focused_window,
            self.cfg.border_thickness,
            self.cfg.header_height,
            self.cfg.app_height,
        )
    }

    /// Best initial cursor: the row of the previously focused app if visible,
    /// otherwise the header of the current desktop, otherwise the top.
    fn initial_cursor(&self, layout: &Layout) -> usize {
        if let Some(f) = self.focused_window {
            for (i, row) in layout.rows.iter().enumerate() {
                if let Row::App { window_idx, .. } = row {
                    if self.windows[*window_idx].id == f {
                        return i;
                    }
                }
            }
        }
        for (i, row) in layout.rows.iter().enumerate() {
            if let Row::Header { desktop_idx, .. } = row {
                if self.desktops[*desktop_idx].index == self.current_desktop {
                    return i;
                }
            }
        }
        0
    }

    fn ensure_cursor_visible(&mut self, layout: &Layout) {
        if layout.rows.is_empty() {
            self.cursor = 0;
            self.scroll = 0.0;
            return;
        }
        if self.cursor >= layout.rows.len() {
            self.cursor = layout.rows.len() - 1;
        }
        let y = layout.row_y[self.cursor];
        let h = layout.row_h[self.cursor];
        if y < self.scroll {
            self.scroll = y;
        } else if y + h > self.scroll + self.viewport_h {
            self.scroll = y + h - self.viewport_h;
        }
        let max_scroll = (layout.content_h - self.viewport_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
    }

    fn move_cursor(&mut self, delta: i32) {
        let layout = self.layout();
        let n = layout.rows.len() as i32;
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor as i32 + delta).clamp(0, n - 1) as usize;
        self.ensure_cursor_visible(&layout);
    }

    fn jump_to(&mut self, idx: usize) {
        let layout = self.layout();
        if layout.rows.is_empty() {
            return;
        }
        self.cursor = idx.min(layout.rows.len() - 1);
        self.ensure_cursor_visible(&layout);
    }

    /// Return the window index of the row under the cursor, or None if the
    /// cursor is on a header.
    fn cursor_window_idx(&self) -> Option<usize> {
        match self.layout().rows.get(self.cursor)? {
            Row::App { window_idx, .. } => Some(*window_idx),
            Row::Header { .. } => None,
        }
    }

    /// Slide the cursor onto the row that hosts `window_idx`, then rescroll.
    /// No-op if the window is gone.
    fn snap_cursor_to_window(&mut self, window_idx: usize) {
        let layout = self.layout();
        for (i, row) in layout.rows.iter().enumerate() {
            if let Row::App { window_idx: wi, .. } = row {
                if *wi == window_idx {
                    self.cursor = i;
                    break;
                }
            }
        }
        self.ensure_cursor_visible(&layout);
    }

    /// Move the app at `window_idx` `delta` desktops. With `follow`, also
    /// switches to that desktop AND activates the moved app — the intent is
    /// "bring this app with me", so the user lands on the new desktop with
    /// the moved app focused. EWMH "sticky" apps (`desktop == u32::MAX`) are
    /// left alone without `follow`; with `follow`, only the desktop changes.
    fn move_app(
        &mut self,
        ew: &Ewmh,
        window_idx: usize,
        delta: i32,
        follow: bool,
        time: u32,
    ) -> Result<()> {
        let n = self.desktops.len() as i32;
        if n == 0 {
            return Ok(());
        }
        let win = &self.windows[window_idx];
        let sticky = win.desktop == u32::MAX;
        if !follow && sticky {
            return Ok(());
        }
        let base = if sticky { self.current_desktop } else { win.desktop } as i32;
        let target = ((base + delta).rem_euclid(n)) as u32;
        let win_id = win.id;
        if !sticky && target != win.desktop {
            ew.move_window_to_desktop(win_id, target)?;
            self.windows[window_idx].desktop = target;
        }
        if follow {
            self.activate_app(ew, win_id, target, time)?;
        } else {
            self.refresh_focus(ew);
        }
        self.snap_cursor_to_window(window_idx);
        Ok(())
    }

    /// Mouse-drop variant: move `window_idx` to absolute `target` desktop,
    /// un-stickying if needed. Activates the moved app when it ends up in
    /// front of the user (either because `follow` brought the user along,
    /// or because the drop landed on the current desktop). Drops to other
    /// desktops without `follow` leave focus alone.
    fn drop_app(
        &mut self,
        ew: &Ewmh,
        window_idx: usize,
        target: u32,
        follow: bool,
        time: u32,
    ) -> Result<()> {
        let win = &self.windows[window_idx];
        let win_id = win.id;
        let sticky = win.desktop == u32::MAX;
        if sticky || win.desktop != target {
            ew.move_window_to_desktop(win_id, target)?;
            self.windows[window_idx].desktop = target;
        }
        // Activate the moved app when it's now in front of the user — either
        // because we followed (shift+drag) or because the drop landed on the
        // current desktop. The plain "drag away" case (target on another
        // desktop) deliberately does NOT activate, so focus doesn't migrate
        // to an invisible window.
        if follow || target == self.current_desktop {
            self.activate_app(ew, win_id, target, time)?;
        } else {
            self.refresh_focus(ew);
        }
        self.snap_cursor_to_window(window_idx);
        Ok(())
    }

    /// Activate `win_id` (now living on `target`) and update local state to
    /// match. We explicitly switch the user to `target` first so that when
    /// `_NET_ACTIVE_WINDOW` arrives the window is already on the user's
    /// current desktop — some WMs otherwise interpret activate-on-other-
    /// desktop as "pull the window to the user", which silently reverts our
    /// preceding `_NET_WM_DESKTOP` move. `_NET_WM_USER_TIME` is bumped first
    /// to defuse focus-stealing prevention; that bump also keeps the
    /// auto-focus race after the desktop switch from picking a different
    /// window than ours.
    fn activate_app(&mut self, ew: &Ewmh, win_id: u32, target: u32, time: u32) -> Result<()> {
        let _ = ew.bump_user_time(win_id, time);
        if target != self.current_desktop {
            ew.switch_desktop(target, time)?;
        }
        ew.activate_window(win_id, time)?;
        // Xfwm4 with `raise_on_focus = false` (and some other WMs) won't
        // raise on activate, so the focused window can stay hidden behind
        // whatever was previously on top of the destination desktop.
        let _ = ew.raise_window(win_id);
        self.current_desktop = target;
        self.focused_window = Some(win_id);
        Ok(())
    }

    /// Re-read `_NET_ACTIVE_WINDOW` after a WM operation that might have
    /// shifted focus (moving the active app off the current desktop, or
    /// switching desktops). On error we keep the stale value rather than
    /// blanking the "focused" badge.
    fn refresh_focus(&mut self, ew: &Ewmh) {
        if let Ok(w) = ew.active_window() {
            self.focused_window = w;
        }
    }

    /// Ask the WM to close the app at `window_idx` (it gets a chance to save).
    /// Drop the row from local state so the popup reflects the kill immediately
    /// and the cursor walks naturally for repeated `dd`s.
    fn close_app(&mut self, ew: &Ewmh, window_idx: usize, time: u32) -> Result<()> {
        let win_id = self.windows[window_idx].id;
        ew.close_window(win_id, time)?;
        self.windows.remove(window_idx);
        if self.focused_window == Some(win_id) {
            self.focused_window = None;
        }
        let layout = self.layout();
        if self.cursor >= layout.rows.len() {
            self.cursor = layout.rows.len().saturating_sub(1);
        }
        self.ensure_cursor_visible(&layout);
        Ok(())
    }

    fn activate_cursor(&mut self, ew: &Ewmh, time: u32) -> Result<()> {
        let layout = self.layout();
        let Some(row) = layout.rows.get(self.cursor) else {
            return Ok(());
        };
        match *row {
            Row::Header { desktop_idx, .. } => {
                ew.switch_desktop(self.desktops[desktop_idx].index, time)?;
            }
            Row::App { window_idx, .. } => {
                let win = &self.windows[window_idx];
                // For sticky windows (`u32::MAX`) the "target" is wherever the
                // user already is — don't switch, just focus.
                let target = if win.desktop == u32::MAX {
                    self.current_desktop
                } else {
                    win.desktop
                };
                let win_id = win.id;
                self.activate_app(ew, win_id, target, time)?;
            }
        }
        Ok(())
    }
}

fn drag_label(w: &WindowInfo) -> String {
    if w.class.is_empty() {
        w.title.clone()
    } else {
        capitalize(&w.class)
    }
}

/// Keycodes for keys currently held — used at popup launch to swallow the
/// autorepeat of the Super+D combo that opened us.
fn held_keycodes(conn: &RustConnection) -> Result<Vec<u8>> {
    let r = conn.query_keymap()?.reply()?;
    let mut held = Vec::new();
    for (byte, b) in r.keys.iter().enumerate() {
        for bit in 0..8 {
            if b & (1 << bit) != 0 {
                held.push((byte * 8 + bit) as u8);
            }
        }
    }
    Ok(held)
}

fn run_popup() -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::snapshot(cfg)?;

    // Size the popup to its content (capped by max_height). The +pad*2 floor
    // guarantees room for at least one header even when there are no apps.
    let layout = state.layout();
    state.cursor = state.initial_cursor(&layout);
    let min_h = state.cfg.header_height + state.cfg.border_thickness * 2.0;
    let content_h = (layout.content_h.ceil() as u16).max(min_h as u16);
    let height = content_h.min(state.cfg.max_height);
    state.viewport_h = height as f64;
    state.ensure_cursor_visible(&layout);
    drop(layout);

    let popup = Popup::open(state.cfg.width, height)?;
    let atoms = Atoms::new(&popup.conn)?;
    let ew = Ewmh::new(&popup.conn, popup.root, &atoms);
    let keymap = Keymap::fetch(&popup.conn)?;
    let mut renderer = Renderer::new(state.cfg.width as i32, height as i32);

    // Swallow any non-modifier keys still held down at launch (the WM may
    // deliver one more KeyPress of the Super+D combo as autorepeat right after
    // we grab the keyboard, which we don't want to interpret as 'd').
    let mut swallow = held_keycodes(&popup.conn)?;
    let mut needs_repaint = true;

    loop {
        if needs_repaint {
            repaint(&popup, &mut renderer, &state)?;
            needs_repaint = false;
        }
        match popup.next_event()? {
            Event::Expose(_) => needs_repaint = true,
            Event::KeyRelease(ev) => {
                swallow.retain(|&kc| kc != ev.detail);
            }
            Event::KeyPress(ev) => {
                if let Some(pos) = swallow.iter().position(|&kc| kc == ev.detail) {
                    swallow.swap_remove(pos);
                    continue;
                }
                let m = u16::from(ev.state);
                let mods = Mods {
                    shift: m & u16::from(KeyButMask::SHIFT) != 0,
                    ctrl: m & u16::from(KeyButMask::CONTROL) != 0,
                    super_: m & u16::from(KeyButMask::MOD4) != 0,
                };
                let key = keymap.lookup(ev.detail, m);
                match handle_keypress(&mut state, &ew, key, ev.time, mods)? {
                    Outcome::Repaint => needs_repaint = true,
                    Outcome::Close => return Ok(()),
                    Outcome::Nothing => {}
                }
            }
            Event::ButtonPress(ev) => {
                if ev.event_x < 0
                    || ev.event_y < 0
                    || ev.event_x >= popup.w as i16
                    || ev.event_y >= popup.h as i16
                {
                    return Ok(()); // click outside the popup
                }
                if state.show_help {
                    continue;
                }
                let layout = state.layout();
                let Some(idx) = layout.row_at_y(ev.event_y as f64, state.scroll) else {
                    continue;
                };
                if ev.detail == 2 {
                    if let Row::App { window_idx, .. } = layout.rows[idx] {
                        state.middle_press = Some(state.windows[window_idx].id);
                    }
                    continue;
                }
                if ev.detail != 1 {
                    continue;
                }
                match layout.rows[idx] {
                    Row::App { window_idx, .. } => {
                        let win = &state.windows[window_idx];
                        state.cursor = idx;
                        let follow = u16::from(ev.state) & u16::from(KeyButMask::SHIFT) != 0;
                        state.drag = Some(Drag {
                            window_id: win.id,
                            label: drag_label(win),
                            cursor_x: ev.event_x as i32,
                            cursor_y: ev.event_y as i32,
                            target_desktop: None,
                            press_x: ev.event_x as i32,
                            press_y: ev.event_y as i32,
                            started: false,
                            follow,
                        });
                        needs_repaint = true;
                    }
                    Row::Header { desktop_idx, .. } => {
                        ew.switch_desktop(state.desktops[desktop_idx].index, ev.time)?;
                        return Ok(());
                    }
                }
            }
            Event::MotionNotify(ev) => {
                if state.drag.is_none() {
                    continue;
                }
                // Resolve the target desktop first so we can drop the &state
                // borrow before mutably touching state.drag.
                let layout = state.layout();
                let target = layout
                    .row_at_y(ev.event_y as f64, state.scroll)
                    .map(|i| match layout.rows[i] {
                        Row::Header { desktop_idx, .. } => state.desktops[desktop_idx].index,
                        Row::App { window_idx, .. } => state.windows[window_idx].desktop,
                    });
                let d = state.drag.as_mut().unwrap();
                d.cursor_x = ev.event_x as i32;
                d.cursor_y = ev.event_y as i32;
                if !d.started && d.moved_enough() {
                    d.started = true;
                }
                d.target_desktop = target;
                needs_repaint = true;
            }
            Event::ButtonRelease(ev) => {
                if ev.detail == 2 {
                    let pressed = state.middle_press.take();
                    if let Some(id) = pressed {
                        let layout = state.layout();
                        if let Some(idx) = layout.row_at_y(ev.event_y as f64, state.scroll) {
                            if let Row::App { window_idx, .. } = layout.rows[idx] {
                                if state.windows[window_idx].id == id {
                                    state.close_app(&ew, window_idx, ev.time)?;
                                    needs_repaint = true;
                                }
                            }
                        }
                    }
                    continue;
                }
                if ev.detail != 1 {
                    continue;
                }
                let Some(d) = state.drag.take() else { continue };
                if d.started {
                    if let Some(target) = d.target_desktop {
                        if let Some(wi) = state.windows.iter().position(|w| w.id == d.window_id) {
                            state.drop_app(&ew, wi, target, d.follow, ev.time)?;
                        }
                    }
                    needs_repaint = true;
                } else {
                    // No real drag → treat as a click on the (already-selected) row.
                    state.activate_cursor(&ew, ev.time)?;
                    return Ok(());
                }
            }
            Event::FocusOut(ev) => {
                if ev.mode == NotifyMode::NORMAL {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Mods {
    shift: bool,
    ctrl: bool,
    super_: bool,
}

enum Outcome {
    Nothing,
    Repaint,
    Close,
}

fn handle_keypress(
    state: &mut State,
    ew: &Ewmh,
    key: Key,
    time: u32,
    mods: Mods,
) -> Result<Outcome> {
    // Help overlay is modal: only F1/Esc dismiss it, and Super+D / q remain
    // unconditional popup-closers. Everything else is ignored.
    if state.show_help {
        let is_super_d =
            matches!(key, Key::Char(c) if c.eq_ignore_ascii_case(&'d')) && mods.super_;
        if is_super_d || matches!(key, Key::Char('q' | 'Q')) {
            return Ok(Outcome::Close);
        }
        if matches!(key, Key::F1 | Key::Escape) {
            state.show_help = false;
            return Ok(Outcome::Repaint);
        }
        return Ok(Outcome::Nothing);
    }

    if matches!(key, Key::F1) {
        state.show_help = true;
        state.pending_dd = false;
        state.pending_gg = false;
        return Ok(Outcome::Repaint);
    }
    if matches!(key, Key::Escape) {
        return Ok(Outcome::Close);
    }

    // Any non-`d` cancels pending `dd`; any non-`g` cancels pending `gg`.
    let is_plain_d =
        matches!(key, Key::Char(c) if c.eq_ignore_ascii_case(&'d')) && !mods.super_;
    let is_plain_g = matches!(key, Key::Char('g'));
    let mut repaint = false;
    if state.pending_dd && !is_plain_d {
        state.pending_dd = false;
        repaint = true;
    }
    if state.pending_gg && !is_plain_g {
        state.pending_gg = false;
        repaint = true;
    }

    if let Key::Char(c) = key {
        // Single-key app hint. Hints fire only with no modifiers — the pool
        // (`f s a r e w t v c x b z h l n m u i o p y`, see render.rs) was
        // chosen to avoid every already-bound letter, so a hint key plus
        // shift/ctrl/super was clearly not aimed at jumping. Lowercase before
        // lookup so caps-lock doesn't disable the feature.
        if !mods.shift && !mods.ctrl && !mods.super_ {
            let layout = state.layout();
            if let Some(row_idx) = layout.row_idx_for_hint(c.to_ascii_lowercase()) {
                state.cursor = row_idx;
                state.activate_cursor(ew, time)?;
                return Ok(Outcome::Close);
            }
        }

        // gg → top. G (shift+g) → bottom. Case-sensitive — `G` is its own key.
        if c == 'g' {
            if state.pending_gg {
                state.pending_gg = false;
                state.jump_to(0);
            } else {
                state.pending_gg = true;
            }
            return Ok(Outcome::Repaint);
        }
        if c == 'G' {
            state.jump_to(usize::MAX);
            return Ok(Outcome::Repaint);
        }

        let lc = c.to_ascii_lowercase();
        if lc == 'd' {
            if mods.super_ {
                return Ok(Outcome::Close); // Super+D toggles the popup off
            }
            if state.pending_dd {
                state.pending_dd = false;
                if let Some(wi) = state.cursor_window_idx() {
                    state.close_app(ew, wi, time)?;
                }
                return Ok(Outcome::Repaint);
            }
            state.pending_dd = true;
            return Ok(Outcome::Repaint);
        }
        if lc == 'q' {
            return Ok(Outcome::Close);
        }
        // 1..9 → desktops 1..9, 0 → desktop 10.
        if let Some(d) = c.to_digit(10) {
            let idx = if d == 0 { 9 } else { (d - 1) as usize };
            if idx < state.desktops.len() {
                ew.switch_desktop(state.desktops[idx].index, time)?;
                return Ok(Outcome::Close);
            }
            return Ok(if repaint { Outcome::Repaint } else { Outcome::Nothing });
        }
        if lc == 'j' || lc == 'k' {
            let delta: i32 = if lc == 'j' { 1 } else { -1 };
            if mods.shift {
                // Shift+J/K: move the selected app one desktop. With Ctrl
                // also held, follow the app to that desktop.
                if let Some(wi) = state.cursor_window_idx() {
                    state.move_app(ew, wi, delta, mods.ctrl, time)?;
                    return Ok(Outcome::Repaint);
                }
                return Ok(if repaint { Outcome::Repaint } else { Outcome::Nothing });
            }
            state.move_cursor(delta);
            return Ok(Outcome::Repaint);
        }
    }

    match key {
        Key::Down => {
            state.move_cursor(1);
            Ok(Outcome::Repaint)
        }
        Key::Up => {
            state.move_cursor(-1);
            Ok(Outcome::Repaint)
        }
        Key::Return => {
            state.activate_cursor(ew, time)?;
            Ok(Outcome::Close)
        }
        _ => Ok(if repaint { Outcome::Repaint } else { Outcome::Nothing }),
    }
}

fn repaint(popup: &Popup, renderer: &mut Renderer, state: &State) -> Result<()> {
    let pixels = if state.show_help {
        renderer.draw_help(&state.cfg)?
    } else {
        let layout = state.layout();
        let overlay = state
            .drag
            .as_ref()
            .filter(|d| d.started)
            .map(|d| DragOverlay {
                cursor_x: d.cursor_x,
                cursor_y: d.cursor_y,
                label: d.label.clone(),
                target_desktop: d.target_desktop,
            });
        renderer.draw(
            &state.cfg,
            &layout,
            &state.desktops,
            &state.windows,
            state.cursor,
            state.scroll,
            overlay.as_ref(),
        )?
    };
    popup.put(&pixels)?;
    popup.flush()?;
    Ok(())
}
