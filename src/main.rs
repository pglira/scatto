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
use crate::render::{DragOverlay, Layout, Renderer, Row};

fn main() -> Result<()> {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "" | "popup" => run_popup(),
        "switch-next" => action_switch(1),
        "switch-prev" => action_switch(-1),
        "move-next" => action_move(1),
        "move-prev" => action_move(-1),
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

fn action_switch(delta: i32) -> Result<()> {
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
    ew.switch_desktop(next, x11rb::CURRENT_TIME)?;
    Ok(())
}

fn action_move(delta: i32) -> Result<()> {
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
    if let Some(active) = ew.active_window()? {
        ew.move_window_to_desktop(active, next)?;
    }
    ew.switch_desktop(next, x11rb::CURRENT_TIME)?;
    Ok(())
}

// -- popup --------------------------------------------------------------------

struct State {
    desktops: Vec<DesktopInfo>,
    windows: Vec<WindowInfo>,
    current_desktop: u32,
    focused_window: Option<u32>,
    cursor: usize,
    scroll: f64,
    drag: Option<Drag>,
    viewport_h: f64,
    /// Vertical padding above the first row and below the last row.
    /// Set to `cfg.border_thickness` so rows sit flush against the border.
    pad_y: f64,
    header_h: f64,
    app_h: f64,
    /// True after the first `d` of a vim-style `dd` close-window chord.
    /// Any non-`d` keypress clears it; the second `d` closes the selected
    /// app's window. `d` with Super held is the popup-toggle and bypasses this.
    pending_dd: bool,
    /// True after the first `g` of a vim-style `gg` jump-to-top chord.
    pending_gg: bool,
    /// True while the F1 key-bindings overlay is shown.
    show_help: bool,
}

#[derive(Clone)]
struct Drag {
    window_id: u32,
    label: String,
    cursor_x: i32,
    cursor_y: i32,
    target_desktop: Option<u32>,
    started: bool, // true once mouse has moved past a small threshold
    press_x: i32,
    press_y: i32,
}

fn run_popup() -> Result<()> {
    let cfg = Config::load()?;

    // Snapshot everything via a temporary connection BEFORE opening the popup,
    // so _NET_ACTIVE_WINDOW still reflects the user's previously focused app.
    let (desktops, windows, current_desktop, focused_window) = {
        let (conn, screen_num) = RustConnection::connect(None).context("X11 connect")?;
        let root = conn.setup().roots[screen_num].root;
        let atoms = Atoms::new(&conn)?;
        let ew = Ewmh::new(&conn, root, &atoms);
        (
            ew.desktops()?,
            ew.windows()?,
            ew.current_desktop()?,
            ew.active_window().ok().flatten(),
        )
    };

    let mut state = State {
        desktops,
        windows,
        current_desktop,
        focused_window,
        cursor: 0,
        scroll: 0.0,
        drag: None,
        viewport_h: 0.0,
        pad_y: cfg.border_thickness,
        header_h: cfg.header_height,
        app_h: cfg.app_height,
        pending_dd: false,
        pending_gg: false,
        show_help: false,
    };

    let layout = Layout::build(
        &state.desktops,
        &state.windows,
        state.current_desktop,
        state.focused_window,
        state.pad_y,
        state.header_h,
        state.app_h,
    );
    state.cursor = initial_cursor(
        &layout,
        &state.desktops,
        &state.windows,
        state.focused_window,
        state.current_desktop,
    );

    let content_h = (layout.content_h.ceil() as u16).max((state.header_h + state.pad_y * 2.0) as u16);
    let height = content_h.min(cfg.max_height);
    state.viewport_h = height as f64;

    let popup = Popup::open(cfg.width, height)?;
    let atoms = Atoms::new(&popup.conn)?;
    let ew_popup = Ewmh::new(&popup.conn, popup.root, &atoms);
    let _ = ew_popup.set_wm_class(popup.win, "scatto", "scatto");
    let keymap = Keymap::fetch(&popup.conn)?;
    let mut renderer = Renderer::new(cfg.width as i32, height as i32);

    ensure_cursor_visible(&layout, &mut state);
    drop(layout);

    // Swallow any non-modifier keys still held down at launch (the WM may
    // deliver one more KeyPress of the Super+D combo as autorepeat right after
    // we grab the keyboard, which we don't want to interpret as 'd').
    let mut swallow: Vec<u8> = {
        let r = popup.conn.query_keymap()?.reply()?;
        let mut held = Vec::new();
        for (byte, b) in r.keys.iter().enumerate() {
            for bit in 0..8 {
                if b & (1 << bit) != 0 {
                    held.push((byte * 8 + bit) as u8);
                }
            }
        }
        held
    };

    let mut needs_repaint = true;

    loop {
        if needs_repaint {
            let layout = Layout::build(
                &state.desktops,
                &state.windows,
                state.current_desktop,
                state.focused_window,
                state.pad_y,
                state.header_h,
                state.app_h,
            );
            ensure_cursor_visible(&layout, &mut state);
            let overlay = state.drag.as_ref().and_then(|d| {
                if !d.started {
                    return None;
                }
                Some(DragOverlay {
                    cursor_x: d.cursor_x,
                    cursor_y: d.cursor_y,
                    label: d.label.clone(),
                    target_desktop: d.target_desktop,
                })
            });
            let pixels = if state.show_help {
                renderer.draw_help(&cfg)?
            } else {
                renderer.draw(
                    &cfg,
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
            needs_repaint = false;
        }

        let event = popup.next_event()?;
        match event {
            Event::Expose(_) => {
                needs_repaint = true;
            }
            Event::KeyRelease(ev) => {
                swallow.retain(|&kc| kc != ev.detail);
            }
            Event::KeyPress(ev) => {
                if let Some(pos) = swallow.iter().position(|&kc| kc == ev.detail) {
                    swallow.swap_remove(pos);
                    continue;
                }
                let state_mask = u16::from(ev.state);
                let shift = state_mask & u16::from(KeyButMask::SHIFT) != 0;
                let ctrl = state_mask & u16::from(KeyButMask::CONTROL) != 0;
                let super_held = state_mask & u16::from(KeyButMask::MOD4) != 0;
                let key = keymap.lookup(ev.detail, state_mask);
                let layout = Layout::build(
                    &state.desktops,
                    &state.windows,
                    state.current_desktop,
                    state.focused_window,
                    state.pad_y,
                    state.header_h,
                    state.app_h,
                );

                // Help overlay is modal: only F1/Esc dismiss it, and Super+D/q
                // remain unconditional popup-closers. Everything else is ignored.
                if state.show_help {
                    let is_super_d = matches!(key, Key::Char(c) if c.eq_ignore_ascii_case(&'d'))
                        && super_held;
                    if is_super_d || matches!(key, Key::Char('q' | 'Q')) {
                        return Ok(());
                    }
                    if matches!(key, Key::F1 | Key::Escape) {
                        state.show_help = false;
                        needs_repaint = true;
                    }
                    continue;
                }
                if matches!(key, Key::F1) {
                    state.show_help = true;
                    state.pending_dd = false;
                    state.pending_gg = false;
                    needs_repaint = true;
                    continue;
                }
                if matches!(key, Key::Escape) {
                    return Ok(());
                }
                // Any non-`d` keypress cancels a pending `dd`.
                let is_plain_d = matches!(key, Key::Char(c) if c.eq_ignore_ascii_case(&'d'))
                    && !super_held;
                if state.pending_dd && !is_plain_d {
                    state.pending_dd = false;
                    needs_repaint = true;
                }
                // Any non-`g` keypress cancels a pending `gg`.
                let is_plain_g = matches!(key, Key::Char('g'));
                if state.pending_gg && !is_plain_g {
                    state.pending_gg = false;
                    needs_repaint = true;
                }
                if let Key::Char(c) = key {
                    // gg: jump to top. G (shift+g): jump to bottom. Case-sensitive.
                    if c == 'g' {
                        if state.pending_gg {
                            state.pending_gg = false;
                            if !layout.rows.is_empty() {
                                state.cursor = 0;
                                ensure_cursor_visible(&layout, &mut state);
                                needs_repaint = true;
                            }
                            continue;
                        }
                        state.pending_gg = true;
                        needs_repaint = true;
                        continue;
                    }
                    if c == 'G' {
                        if !layout.rows.is_empty() {
                            state.cursor = layout.rows.len() - 1;
                            ensure_cursor_visible(&layout, &mut state);
                            needs_repaint = true;
                        }
                        continue;
                    }
                    let lc = c.to_ascii_lowercase();
                    if lc == 'd' {
                        if super_held {
                            // Super+D pressed again — toggle the popup off.
                            return Ok(());
                        }
                        // Vim-style dd: first `d` arms, second `d` closes the
                        // app at the cursor.
                        if state.pending_dd {
                            state.pending_dd = false;
                            if let Some(Row::App { window_idx, .. }) = layout.rows.get(state.cursor) {
                                let wi = *window_idx;
                                close_selected_window(&mut state, &ew_popup, wi, ev.time)?;
                                needs_repaint = true;
                            }
                            continue;
                        }
                        state.pending_dd = true;
                        needs_repaint = true;
                        continue;
                    }
                    if lc == 'q' {
                        return Ok(());
                    }
                    // Number keys: 1..9 jump to desktops 1..9, 0 jumps to desktop 10.
                    if let Some(d) = c.to_digit(10) {
                        let idx = if d == 0 { 9 } else { (d - 1) as usize };
                        if idx < state.desktops.len() {
                            ew_popup.switch_desktop(state.desktops[idx].index, ev.time)?;
                            return Ok(());
                        }
                        continue;
                    }
                    if lc == 'j' || lc == 'k' {
                        let delta: i32 = if lc == 'j' { 1 } else { -1 };
                        // Shift+Ctrl+J/K on an app row: move that app one
                        // desktop AND follow it (popup stays open). Checked
                        // first so the plain-Shift branch below doesn't catch it.
                        if shift && ctrl {
                            if let Some(Row::App { window_idx, .. }) = layout.rows.get(state.cursor) {
                                let wi = *window_idx;
                                move_selected_app_and_follow(
                                    &mut state, &ew_popup, wi, delta, ev.time,
                                )?;
                                needs_repaint = true;
                            }
                            continue;
                        }
                        // Shift+J/K: move the cursor-selected app one desktop,
                        // popup stays open. On a header (nothing to move),
                        // ignore the chord.
                        if shift {
                            if let Some(Row::App { window_idx, .. }) = layout.rows.get(state.cursor) {
                                let wi = *window_idx;
                                move_selected_app(&mut state, &ew_popup, wi, delta)?;
                                needs_repaint = true;
                            }
                            continue;
                        }
                        move_cursor(&layout, &mut state, delta);
                        needs_repaint = true;
                        continue;
                    }
                }
                match key {
                    Key::Down => {
                        move_cursor(&layout, &mut state, 1);
                        needs_repaint = true;
                    }
                    Key::Up => {
                        move_cursor(&layout, &mut state, -1);
                        needs_repaint = true;
                    }
                    Key::Return => {
                        activate_cursor(&popup, &ew_popup, &layout, &state, ev.time)?;
                        return Ok(());
                    }
                    _ => {}
                }
            }
            Event::ButtonPress(ev) => {
                // Click outside our window? Close.
                if ev.event_x < 0
                    || ev.event_y < 0
                    || ev.event_x >= popup.w as i16
                    || ev.event_y >= popup.h as i16
                {
                    return Ok(());
                }
                // Inside clicks are ignored while the help overlay is up.
                if state.show_help {
                    continue;
                }
                if ev.detail == 1 {
                    let layout = Layout::build(
                        &state.desktops,
                        &state.windows,
                        state.current_desktop,
                        state.focused_window,
                        state.pad_y,
                        state.header_h,
                        state.app_h,
                    );
                    if let Some(idx) = layout.row_at_y(ev.event_y as f64, state.scroll) {
                        match layout.rows[idx] {
                            Row::App { window_idx, .. } => {
                                let win = &state.windows[window_idx];
                                state.cursor = idx;
                                state.drag = Some(Drag {
                                    window_id: win.id,
                                    label: drag_label(win),
                                    cursor_x: ev.event_x as i32,
                                    cursor_y: ev.event_y as i32,
                                    target_desktop: None,
                                    started: false,
                                    press_x: ev.event_x as i32,
                                    press_y: ev.event_y as i32,
                                });
                                needs_repaint = true;
                            }
                            Row::Header { desktop_idx, .. } => {
                                // Click on a desktop header → switch and close.
                                ew_popup.switch_desktop(state.desktops[desktop_idx].index, ev.time)?;
                                return Ok(());
                            }
                        }
                    }
                }
            }
            Event::MotionNotify(ev) => {
                if let Some(d) = state.drag.as_mut() {
                    d.cursor_x = ev.event_x as i32;
                    d.cursor_y = ev.event_y as i32;
                    let dx = d.cursor_x - d.press_x;
                    let dy = d.cursor_y - d.press_y;
                    if !d.started && (dx * dx + dy * dy) > 16 {
                        d.started = true;
                    }
                    let layout = Layout::build(
                        &state.desktops,
                        &state.windows,
                        state.current_desktop,
                        state.focused_window,
                        state.pad_y,
                        state.header_h,
                        state.app_h,
                    );
                    d.target_desktop =
                        layout.row_at_y(ev.event_y as f64, state.scroll).and_then(|i| {
                            match layout.rows[i] {
                                Row::Header { desktop_idx, .. } => {
                                    Some(state.desktops[desktop_idx].index)
                                }
                                Row::App { window_idx, .. } => {
                                    Some(state.windows[window_idx].desktop)
                                }
                            }
                        });
                    needs_repaint = true;
                }
            }
            Event::ButtonRelease(ev) => {
                if ev.detail != 1 {
                    continue;
                }
                if let Some(d) = state.drag.take() {
                    if d.started {
                        if let Some(target) = d.target_desktop {
                            ew_popup.move_window_to_desktop(d.window_id, target)?;
                            return Ok(());
                        }
                        needs_repaint = true;
                    } else {
                        // Treat as click: activate the row.
                        let layout = Layout::build(
                            &state.desktops,
                            &state.windows,
                            state.current_desktop,
                            state.focused_window,
                            state.pad_y,
                            state.header_h,
                            state.app_h,
                        );
                        activate_cursor(&popup, &ew_popup, &layout, &state, ev.time)?;
                        return Ok(());
                    }
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

fn initial_cursor(
    layout: &Layout,
    desktops: &[DesktopInfo],
    windows: &[WindowInfo],
    focused: Option<u32>,
    current_desktop: u32,
) -> usize {
    if let Some(f) = focused {
        for (i, row) in layout.rows.iter().enumerate() {
            if let Row::App { window_idx, .. } = row {
                if windows[*window_idx].id == f {
                    return i;
                }
            }
        }
    }
    for (i, row) in layout.rows.iter().enumerate() {
        if let Row::Header { desktop_idx, .. } = row {
            if desktops[*desktop_idx].index == current_desktop {
                return i;
            }
        }
    }
    0
}

fn move_cursor(layout: &Layout, state: &mut State, delta: i32) {
    let n = layout.rows.len() as i32;
    if n == 0 {
        return;
    }
    let mut c = state.cursor as i32 + delta;
    if c < 0 {
        c = 0;
    }
    if c >= n {
        c = n - 1;
    }
    state.cursor = c as usize;
    ensure_cursor_visible(layout, state);
}

fn ensure_cursor_visible(layout: &Layout, state: &mut State) {
    if state.cursor >= layout.rows.len() {
        state.cursor = layout.rows.len().saturating_sub(1);
    }
    if layout.rows.is_empty() {
        state.scroll = 0.0;
        return;
    }
    let y = layout.row_y[state.cursor];
    let h = layout.row_h[state.cursor];
    let viewport = state.viewport_h;
    if y < state.scroll {
        state.scroll = y;
    } else if y + h > state.scroll + viewport {
        state.scroll = y + h - viewport;
    }
    let max_scroll = (layout.content_h - viewport).max(0.0);
    if state.scroll > max_scroll {
        state.scroll = max_scroll;
    }
    if state.scroll < 0.0 {
        state.scroll = 0.0;
    }
}

/// Move the app at `window_idx` one desktop in `delta` direction. Updates
/// local state so the next repaint reflects the move, and slides `state.cursor`
/// onto the moved app's new row so the user can keep moving it further.
fn move_selected_app(
    state: &mut State,
    ew: &Ewmh,
    window_idx: usize,
    delta: i32,
) -> Result<()> {
    let n = state.desktops.len() as i32;
    if n == 0 {
        return Ok(());
    }
    let win = &state.windows[window_idx];
    // EWMH "sticky" windows live on every desktop — skip the move.
    if win.desktop == u32::MAX {
        return Ok(());
    }
    let new_d = ((win.desktop as i32 + delta).rem_euclid(n)) as u32;
    if new_d == win.desktop {
        return Ok(());
    }
    ew.move_window_to_desktop(win.id, new_d)?;
    state.windows[window_idx].desktop = new_d;

    let layout = Layout::build(
        &state.desktops,
        &state.windows,
        state.current_desktop,
        state.focused_window,
        state.pad_y,
        state.header_h,
        state.app_h,
    );
    for (i, row) in layout.rows.iter().enumerate() {
        if let Row::App { window_idx: wi, .. } = row {
            if *wi == window_idx {
                state.cursor = i;
                break;
            }
        }
    }
    ensure_cursor_visible(&layout, state);
    Ok(())
}

/// Move the app at `window_idx` one desktop in `delta`, then switch to that
/// desktop. Popup stays open: local state is updated and `state.cursor` moves
/// onto the app's new row so the user can keep shoving it further.
fn move_selected_app_and_follow(
    state: &mut State,
    ew: &Ewmh,
    window_idx: usize,
    delta: i32,
    time: u32,
) -> Result<()> {
    let n = state.desktops.len() as i32;
    if n == 0 {
        return Ok(());
    }
    let win = &state.windows[window_idx];
    let target = if win.desktop == u32::MAX {
        // Sticky — already on every desktop. Step from the current one.
        ((state.current_desktop as i32 + delta).rem_euclid(n)) as u32
    } else {
        ((win.desktop as i32 + delta).rem_euclid(n)) as u32
    };

    if win.desktop != u32::MAX && target != win.desktop {
        ew.move_window_to_desktop(win.id, target)?;
        state.windows[window_idx].desktop = target;
    }
    if target != state.current_desktop {
        ew.switch_desktop(target, time)?;
        state.current_desktop = target;
    }

    let layout = Layout::build(
        &state.desktops,
        &state.windows,
        state.current_desktop,
        state.focused_window,
        state.pad_y,
        state.header_h,
        state.app_h,
    );
    for (i, row) in layout.rows.iter().enumerate() {
        if let Row::App { window_idx: wi, .. } = row {
            if *wi == window_idx {
                state.cursor = i;
                break;
            }
        }
    }
    ensure_cursor_visible(&layout, state);
    Ok(())
}

/// Send `_NET_CLOSE_WINDOW` to the app at `window_idx`, drop it from local
/// state so the popup reflects the kill immediately, and keep the cursor on
/// the same row index (clamped) so consecutive `dd`s walk down the list.
fn close_selected_window(
    state: &mut State,
    ew: &Ewmh,
    window_idx: usize,
    time: u32,
) -> Result<()> {
    let win_id = state.windows[window_idx].id;
    ew.close_window(win_id, time)?;
    state.windows.remove(window_idx);
    if state.focused_window == Some(win_id) {
        state.focused_window = None;
    }
    let layout = Layout::build(
        &state.desktops,
        &state.windows,
        state.current_desktop,
        state.focused_window,
        state.pad_y,
        state.header_h,
        state.app_h,
    );
    if state.cursor >= layout.rows.len() {
        state.cursor = layout.rows.len().saturating_sub(1);
    }
    ensure_cursor_visible(&layout, state);
    Ok(())
}

fn activate_cursor(_popup: &Popup, ew: &Ewmh, layout: &Layout, state: &State, time: u32) -> Result<()> {
    if let Some(row) = layout.rows.get(state.cursor) {
        match row {
            Row::Header { desktop_idx, .. } => {
                ew.switch_desktop(state.desktops[*desktop_idx].index, time)?;
            }
            Row::App { window_idx, .. } => {
                let win = &state.windows[*window_idx];
                // Jump to the app's desktop instead of pulling the app to the
                // current one. u32::MAX means "sticky / on all desktops" —
                // skip the switch in that case.
                if win.desktop != u32::MAX && win.desktop != state.current_desktop {
                    ew.switch_desktop(win.desktop, time)?;
                }
                ew.activate_window(win.id, time)?;
            }
        }
    }
    Ok(())
}

fn drag_label(w: &WindowInfo) -> String {
    if !w.class.is_empty() {
        let mut chars = w.class.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    } else {
        w.title.clone()
    }
}

