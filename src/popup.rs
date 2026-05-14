//! Override-redirect popup window: ARGB32 visual when one is available (so a
//! compositor can blur/dim our background), keyboard + pointer grabbed for the
//! lifetime of the popup, blitting raw cairo pixels via XPutImage.

use anyhow::{anyhow, Context, Result};
use std::borrow::Cow;
use x11rb::connection::Connection;
use x11rb::image::{BitsPerPixel, Image, ImageOrder as XImageOrder, ScanlinePad};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct Geometry {
    pub x: i16,
    pub y: i16,
    pub w: u16,
    pub h: u16,
}

pub struct Popup {
    pub conn: RustConnection,
    pub root: Window,
    pub w: u16,
    pub h: u16,
    win: Window,
    gc: u32,
    depth: u8,
    byte_order: XImageOrder,
}

impl Popup {
    pub fn open(width: u16, height: u16) -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None).context("X11 connect")?;
        let setup = conn.setup().clone();
        let screen = &setup.roots[screen_num];
        let root = screen.root;

        let mon = monitor_under_pointer(&conn, root)?.unwrap_or(Geometry {
            x: 0,
            y: 0,
            w: screen.width_in_pixels,
            h: screen.height_in_pixels,
        });
        let x = mon.x + ((mon.w as i32 - width as i32) / 2) as i16;
        let y = mon.y + ((mon.h as i32 - height as i32) / 2) as i16;

        // Try for an ARGB32 visual so we can render with alpha.
        let (depth, visual_id, use_colormap) =
            find_argb_visual(screen).unwrap_or((screen.root_depth, screen.root_visual, false));

        let colormap = if use_colormap {
            let id = conn.generate_id()?;
            conn.create_colormap(ColormapAlloc::NONE, id, root, visual_id)?;
            id
        } else {
            x11rb::NONE
        };

        let win = conn.generate_id()?;
        let mut aux = CreateWindowAux::new()
            .background_pixel(0)
            .border_pixel(0)
            .override_redirect(1)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::FOCUS_CHANGE,
            );
        if colormap != x11rb::NONE {
            aux = aux.colormap(colormap);
        }
        conn.create_window(
            depth,
            win,
            root,
            x,
            y,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            visual_id,
            &aux,
        )?;

        set_window_type_dialog(&conn, win)?;
        set_wm_class(&conn, win, "scatto", "scatto")?;
        set_wm_name(&conn, win, "scatto")?;

        let gc = conn.generate_id()?;
        conn.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))?;

        conn.map_window(win)?;
        conn.flush()?;

        // Grab keyboard. Retry briefly in case something else has it (e.g. the
        // user is still releasing the launch hotkey).
        let mut got_kb = false;
        for _ in 0..50 {
            let r = conn
                .grab_keyboard(false, win, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)?
                .reply()?;
            if r.status == GrabStatus::SUCCESS {
                got_kb = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !got_kb {
            return Err(anyhow!("could not grab keyboard"));
        }

        // Grab pointer so we receive button + motion events even outside our
        // window — required for drag-and-drop and for closing on outside-click.
        let pointer_events =
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
        let _ = conn
            .grab_pointer(
                false,
                win,
                pointer_events,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )?
            .reply()?;

        Ok(Self {
            conn,
            root,
            win,
            gc,
            w: width,
            h: height,
            depth,
            byte_order: setup.image_byte_order.try_into().unwrap_or(XImageOrder::LsbFirst),
        })
    }

    pub fn put(&self, buf: &[u8]) -> Result<()> {
        let img = Image::new(
            self.w,
            self.h,
            ScanlinePad::Pad32,
            self.depth,
            BitsPerPixel::B32,
            self.byte_order,
            Cow::Borrowed(buf),
        )
        .map_err(|e| anyhow!("Image::new: {e:?}"))?;
        let cookies = img.put(&self.conn, self.win, self.gc, 0, 0)?;
        for c in cookies {
            c.check()?;
        }
        Ok(())
    }

    pub fn next_event(&self) -> Result<Event> {
        Ok(self.conn.wait_for_event()?)
    }

    pub fn flush(&self) -> Result<()> {
        self.conn.flush()?;
        Ok(())
    }
}

fn find_argb_visual(screen: &Screen) -> Option<(u8, u32, bool)> {
    for d in &screen.allowed_depths {
        if d.depth == 32 {
            for v in &d.visuals {
                if v.class == VisualClass::TRUE_COLOR {
                    return Some((32, v.visual_id, true));
                }
            }
        }
    }
    None
}

fn monitor_under_pointer(conn: &RustConnection, root: u32) -> Result<Option<Geometry>> {
    if let Ok(reply) = conn.randr_get_monitors(root, true)?.reply() {
        if !reply.monitors.is_empty() {
            let pq = conn.query_pointer(root)?.reply()?;
            for m in &reply.monitors {
                let inside = pq.root_x >= m.x
                    && pq.root_x < m.x + m.width as i16
                    && pq.root_y >= m.y
                    && pq.root_y < m.y + m.height as i16;
                if inside {
                    return Ok(Some(Geometry { x: m.x, y: m.y, w: m.width, h: m.height }));
                }
            }
            let primary = reply.monitors.iter().find(|m| m.primary).unwrap_or(&reply.monitors[0]);
            return Ok(Some(Geometry { x: primary.x, y: primary.y, w: primary.width, h: primary.height }));
        }
    }
    Ok(None)
}

fn set_wm_name(conn: &RustConnection, win: u32, name: &str) -> Result<()> {
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        name.as_bytes(),
    )?;
    Ok(())
}

fn set_wm_class(conn: &RustConnection, win: u32, instance: &str, class: &str) -> Result<()> {
    let mut data: Vec<u8> = Vec::with_capacity(instance.len() + class.len() + 2);
    data.extend_from_slice(instance.as_bytes());
    data.push(0);
    data.extend_from_slice(class.as_bytes());
    data.push(0);
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        &data,
    )?;
    Ok(())
}

fn set_window_type_dialog(conn: &RustConnection, win: u32) -> Result<()> {
    let net_wm_window_type = conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE")?.reply()?.atom;
    let net_wm_window_type_dialog = conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DIALOG")?.reply()?.atom;
    conn.change_property32(
        PropMode::REPLACE,
        win,
        net_wm_window_type,
        AtomEnum::ATOM,
        &[net_wm_window_type_dialog],
    )?;
    Ok(())
}
