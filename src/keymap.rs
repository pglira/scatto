//! Minimal keycode -> keysym -> Key decoder (lifted from gmenu, trimmed to
//! the keys this program cares about).

use anyhow::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, KeyButMask};

pub struct Keymap {
    min_keycode: u8,
    keysyms_per_keycode: u8,
    table: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Return,
    Escape,
    Up,
    Down,
    Other,
}

impl Keymap {
    pub fn fetch<C: Connection>(conn: &C) -> Result<Self> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let count = max - min + 1;
        let r = conn.get_keyboard_mapping(min, count)?.reply()?;
        Ok(Self {
            min_keycode: min,
            keysyms_per_keycode: r.keysyms_per_keycode,
            table: r.keysyms,
        })
    }

    pub fn lookup(&self, keycode: u8, state: u16) -> Key {
        if keycode < self.min_keycode {
            return Key::Other;
        }
        let idx = (keycode - self.min_keycode) as usize * self.keysyms_per_keycode as usize;
        if idx >= self.table.len() {
            return Key::Other;
        }
        let group_size = self.keysyms_per_keycode.min(4) as usize;
        let shift = (state & u16::from(KeyButMask::SHIFT)) != 0;
        let lock = (state & u16::from(KeyButMask::LOCK)) != 0;
        let col = if shift ^ (lock && self.is_letter_keycode(keycode)) { 1 } else { 0 };
        let col = col.min(group_size.saturating_sub(1));
        let sym = self.table[idx + col];
        let sym0 = self.table[idx];
        let sym = if sym == 0 { sym0 } else { sym };
        keysym_to_key(sym)
    }

    fn is_letter_keycode(&self, keycode: u8) -> bool {
        let idx = (keycode - self.min_keycode) as usize * self.keysyms_per_keycode as usize;
        if idx >= self.table.len() {
            return false;
        }
        let s = self.table[idx];
        (0x61..=0x7a).contains(&s)
    }
}

fn keysym_to_key(sym: u32) -> Key {
    match sym {
        0xff0d | 0xff8d => Key::Return,
        0xff1b => Key::Escape,
        0xff52 => Key::Up,
        0xff54 => Key::Down,
        s if (0x20..0x7f).contains(&s) => Key::Char(char::from(s as u8)),
        _ => Key::Other,
    }
}
