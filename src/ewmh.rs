//! EWMH/ICCCM helpers: read desktop + window state and send the standard
//! client-message requests to ask the window manager to change them.

use anyhow::{anyhow, Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, GetPropertyReply, PropMode,
    Window, CLIENT_MESSAGE_EVENT,
};
use x11rb::rust_connection::RustConnection;

const SOURCE_PAGER: u32 = 2;

pub struct Atoms {
    pub net_number_of_desktops: u32,
    pub net_current_desktop: u32,
    pub net_desktop_names: u32,
    pub net_client_list: u32,
    pub net_client_list_stacking: u32,
    pub net_active_window: u32,
    pub net_wm_desktop: u32,
    pub net_wm_name: u32,
    pub net_wm_icon: u32,
    pub net_wm_window_type: u32,
    pub net_wm_window_type_normal: u32,
    pub net_wm_state: u32,
    pub net_wm_state_skip_taskbar: u32,
    pub net_close_window: u32,
    pub net_wm_user_time: u32,
    pub utf8_string: u32,
}

impl Atoms {
    pub fn new(conn: &RustConnection) -> Result<Self> {
        let i = |name: &[u8]| -> Result<u32> {
            Ok(conn.intern_atom(false, name)?.reply()?.atom)
        };
        Ok(Self {
            net_number_of_desktops: i(b"_NET_NUMBER_OF_DESKTOPS")?,
            net_current_desktop: i(b"_NET_CURRENT_DESKTOP")?,
            net_desktop_names: i(b"_NET_DESKTOP_NAMES")?,
            net_client_list: i(b"_NET_CLIENT_LIST")?,
            net_client_list_stacking: i(b"_NET_CLIENT_LIST_STACKING")?,
            net_active_window: i(b"_NET_ACTIVE_WINDOW")?,
            net_wm_desktop: i(b"_NET_WM_DESKTOP")?,
            net_wm_name: i(b"_NET_WM_NAME")?,
            net_wm_icon: i(b"_NET_WM_ICON")?,
            net_wm_window_type: i(b"_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_normal: i(b"_NET_WM_WINDOW_TYPE_NORMAL")?,
            net_wm_state: i(b"_NET_WM_STATE")?,
            net_wm_state_skip_taskbar: i(b"_NET_WM_STATE_SKIP_TASKBAR")?,
            net_close_window: i(b"_NET_CLOSE_WINDOW")?,
            net_wm_user_time: i(b"_NET_WM_USER_TIME")?,
            utf8_string: i(b"UTF8_STRING")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DesktopInfo {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: Window,
    pub desktop: u32, // u32::MAX means "all desktops" (EWMH sticky)
    pub title: String,
    pub class: String, // WM_CLASS res_class — what we render as "program name"
    pub icon: Option<Vec<u32>>, // raw _NET_WM_ICON contents
}

pub struct Ewmh<'a> {
    conn: &'a RustConnection,
    root: Window,
    atoms: &'a Atoms,
}

impl<'a> Ewmh<'a> {
    pub fn new(conn: &'a RustConnection, root: Window, atoms: &'a Atoms) -> Self {
        Self { conn, root, atoms }
    }

    pub fn number_of_desktops(&self) -> Result<u32> {
        let r = self.get_prop(self.root, self.atoms.net_number_of_desktops, AtomEnum::CARDINAL.into(), 4)?;
        Ok(read_u32(&r).unwrap_or(1))
    }

    pub fn current_desktop(&self) -> Result<u32> {
        let r = self.get_prop(self.root, self.atoms.net_current_desktop, AtomEnum::CARDINAL.into(), 4)?;
        Ok(read_u32(&r).unwrap_or(0))
    }

    fn desktop_names(&self) -> Result<Vec<String>> {
        let r = self.get_prop(self.root, self.atoms.net_desktop_names, self.atoms.utf8_string, u32::MAX / 4)?;
        if r.value_len == 0 {
            return Ok(Vec::new());
        }
        // _NET_DESKTOP_NAMES is a null-separated list of UTF-8 strings.
        let mut out = Vec::new();
        for chunk in r.value.split(|b| *b == 0) {
            if chunk.is_empty() && out.last().map(|s: &String| s.is_empty()).unwrap_or(false) {
                continue;
            }
            out.push(String::from_utf8_lossy(chunk).into_owned());
        }
        // Trim trailing empty entry from the final NUL.
        if out.last().map(|s| s.is_empty()).unwrap_or(false) {
            out.pop();
        }
        Ok(out)
    }

    pub fn desktops(&self) -> Result<Vec<DesktopInfo>> {
        let n = self.number_of_desktops()?;
        let names = self.desktop_names().unwrap_or_default();
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let name = names
                .get(i as usize)
                .cloned()
                .unwrap_or_else(|| format!("Desktop {}", i + 1));
            out.push(DesktopInfo { index: i, name });
        }
        Ok(out)
    }

    pub fn active_window(&self) -> Result<Option<Window>> {
        let r = self.get_prop(self.root, self.atoms.net_active_window, AtomEnum::WINDOW.into(), 4)?;
        Ok(read_u32(&r).filter(|w| *w != 0))
    }

    fn client_list(&self) -> Result<Vec<Window>> {
        // Prefer stacking order (top-of-stack last) so the per-desktop list
        // reflects the user's recency.
        let r = self.get_prop(
            self.root,
            self.atoms.net_client_list_stacking,
            AtomEnum::WINDOW.into(),
            u32::MAX / 4,
        )?;
        let list = read_u32_array(&r);
        if !list.is_empty() {
            return Ok(list);
        }
        let r = self.get_prop(self.root, self.atoms.net_client_list, AtomEnum::WINDOW.into(), u32::MAX / 4)?;
        Ok(read_u32_array(&r))
    }

    pub fn windows(&self) -> Result<Vec<WindowInfo>> {
        let ids = self.client_list()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            // Filter out windows that should not appear in a window switcher.
            if self.has_state(id, self.atoms.net_wm_state_skip_taskbar)? {
                continue;
            }
            if !self.is_normal_or_unspecified(id)? {
                continue;
            }
            let desktop = self.window_desktop(id).unwrap_or(0);
            let title = self.window_title(id).unwrap_or_default();
            let class = self.window_class(id).unwrap_or_default();
            let icon = self.window_icon(id).ok().flatten();
            // Drop windows we can't name at all — they're usually utility/transient
            // things that escaped the type filter.
            if title.is_empty() && class.is_empty() {
                continue;
            }
            out.push(WindowInfo { id, desktop, title, class, icon });
        }
        Ok(out)
    }

    fn window_desktop(&self, w: Window) -> Result<u32> {
        let r = self.get_prop(w, self.atoms.net_wm_desktop, AtomEnum::CARDINAL.into(), 4)?;
        Ok(read_u32(&r).unwrap_or(0))
    }

    fn window_title(&self, w: Window) -> Result<String> {
        let r = self.get_prop(w, self.atoms.net_wm_name, self.atoms.utf8_string, u32::MAX / 4)?;
        if r.value_len > 0 {
            return Ok(String::from_utf8_lossy(&r.value).into_owned());
        }
        let r = self.get_prop(w, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into(), u32::MAX / 4)?;
        if r.value_len > 0 {
            return Ok(String::from_utf8_lossy(&r.value).into_owned());
        }
        Ok(String::new())
    }

    fn window_class(&self, w: Window) -> Result<String> {
        // WM_CLASS is "instance\0class\0" in STRING (Latin-1).
        let r = self.get_prop(w, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into(), 1024)?;
        if r.value_len == 0 {
            return Ok(String::new());
        }
        let parts: Vec<&[u8]> = r.value.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
        // Prefer res_class (second), fall back to res_name (first).
        let pick = parts.get(1).or_else(|| parts.first()).copied().unwrap_or(b"");
        Ok(String::from_utf8_lossy(pick).into_owned())
    }

    fn window_icon(&self, w: Window) -> Result<Option<Vec<u32>>> {
        let r = self.get_prop(w, self.atoms.net_wm_icon, AtomEnum::CARDINAL.into(), u32::MAX / 4)?;
        if r.value_len == 0 || r.format != 32 {
            return Ok(None);
        }
        Ok(Some(read_u32_array(&r)))
    }

    fn has_state(&self, w: Window, state_atom: u32) -> Result<bool> {
        let r = self.get_prop(w, self.atoms.net_wm_state, AtomEnum::ATOM.into(), 1024)?;
        for a in read_u32_array(&r) {
            if a == state_atom {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn is_normal_or_unspecified(&self, w: Window) -> Result<bool> {
        let r = self.get_prop(w, self.atoms.net_wm_window_type, AtomEnum::ATOM.into(), 1024)?;
        let types = read_u32_array(&r);
        if types.is_empty() {
            return Ok(true);
        }
        Ok(types.contains(&self.atoms.net_wm_window_type_normal))
    }

    pub fn switch_desktop(&self, index: u32, time: u32) -> Result<()> {
        self.send_root_message(self.atoms.net_current_desktop, [index, time, 0, 0, 0])
    }

    pub fn move_window_to_desktop(&self, w: Window, index: u32) -> Result<()> {
        self.send_message(self.root, w, self.atoms.net_wm_desktop, [index, SOURCE_PAGER, 0, 0, 0])
    }

    pub fn activate_window(&self, w: Window, time: u32) -> Result<()> {
        self.send_message(self.root, w, self.atoms.net_active_window, [SOURCE_PAGER, time, 0, 0, 0])
    }

    /// Raise `w` to the top of its stack. `_NET_ACTIVE_WINDOW` only changes
    /// focus — Xfwm4 (with `raise_on_focus = false`) and some other WMs leave
    /// stacking untouched, so the activated window can stay hidden behind
    /// the previously-raised one on the destination desktop. An explicit
    /// `ConfigureWindow` with `StackMode::ABOVE` makes the activation
    /// visually match the focus state.
    pub fn raise_window(&self, w: Window) -> Result<()> {
        use x11rb::protocol::xproto::{ConfigureWindowAux, StackMode};
        self.conn
            .configure_window(w, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))
            .context("configure_window raise")?;
        self.conn.flush()?;
        Ok(())
    }

    /// Mark `w` as having been interacted with at `time` (writes
    /// `_NET_WM_USER_TIME` on the target). Many WMs gate `_NET_ACTIVE_WINDOW`
    /// on this property — without a fresh user-time the activate request can
    /// be downgraded to a "demands attention" hint or silently dropped.
    pub fn bump_user_time(&self, w: Window, time: u32) -> Result<()> {
        self.conn
            .change_property(
                PropMode::REPLACE,
                w,
                self.atoms.net_wm_user_time,
                u32::from(AtomEnum::CARDINAL),
                32,
                1,
                &time.to_ne_bytes(),
            )
            .context("change_property _NET_WM_USER_TIME")?;
        self.conn.flush()?;
        Ok(())
    }

    /// Ask the WM to close `w` politely (the client gets a chance to save).
    pub fn close_window(&self, w: Window, time: u32) -> Result<()> {
        self.send_message(self.root, w, self.atoms.net_close_window, [time, SOURCE_PAGER, 0, 0, 0])
    }

    fn send_root_message(&self, message_type: u32, data: [u32; 5]) -> Result<()> {
        self.send_message(self.root, self.root, message_type, data)
    }

    fn send_message(&self, dest: Window, target: Window, message_type: u32, data: [u32; 5]) -> Result<()> {
        let ev = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: target,
            type_: message_type,
            data: data.into(),
        };
        self.conn
            .send_event(
                false,
                dest,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                ev,
            )
            .context("send_event")?
            .check()
            .context("send_event check")?;
        self.conn.flush()?;
        Ok(())
    }

    fn get_prop(&self, w: Window, prop: u32, ty: u32, len: u32) -> Result<GetPropertyReply> {
        self.conn
            .get_property(false, w, prop, ty, 0, len)
            .map_err(|e| anyhow!("get_property: {e}"))?
            .reply()
            .map_err(|e| anyhow!("get_property reply: {e}"))
    }

}

fn read_u32(r: &GetPropertyReply) -> Option<u32> {
    if r.format != 32 || r.value_len == 0 {
        return None;
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&r.value[..4]);
    Some(u32::from_ne_bytes(buf))
}

fn read_u32_array(r: &GetPropertyReply) -> Vec<u32> {
    if r.format != 32 || r.value_len == 0 {
        return Vec::new();
    }
    r.value
        .chunks_exact(4)
        .map(|c| {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(c);
            u32::from_ne_bytes(buf)
        })
        .collect()
}
