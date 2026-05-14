# scatto

A centered X11 popup that shows your desktops and the apps on each, with
vim-style window-moving by keys or drag-and-drop. Built on the same stack
as gmenu (Rust + x11rb + cairo/pango), so the same system libraries cover
both.

## What it does

Press **Super+D** to toggle a centered popup on the monitor under the
pointer:

```
● main                       <- current desktop
    [icon] Firefox  github.com - Mozilla Firefox
    [icon] Kitty   nvim ./src/main.rs
○ code
    [icon] Code    scatto
○ chat                       <- empty desktop
```

### Closing the popup

- **Super+D** again, **Esc**, click-outside, or focus-loss.

### Navigation

- **j / k** (or **Down / Up**): move the cursor through the list.

### Switching desktops

- **Enter** on a header → switch to that desktop. Closes the popup.
- **Enter** on an app row → jump to that app's desktop AND focus the app.
  Closes the popup. (The app stays on its own desktop; the popup does not
  pull it to wherever you currently are.)
- **1–9** / **0** → jump straight to desktop 1–9 / 10. Closes the popup.
  Out-of-range digits are ignored.
- **Click** on a header does the same as Enter on a header.

### Moving the cursor-selected app

- **Shift+J / Shift+K** on an *app* row → move THAT app one desktop down /
  up. Popup stays open, cursor follows the app, so you can keep pressing to
  shove it further. On a header row, the chord is ignored.
- **Shift+Ctrl+J / Shift+Ctrl+K** on an *app* row → move that app one
  desktop down / up AND switch to that desktop. Popup stays open, cursor
  follows.
- **Drag** an app row with the left mouse button onto a desktop header to
  move that window to that desktop. The target gets a green underline; the
  popup closes after the drop.

### Closing an app

- **dd** (vim-style — two `d` presses in a row) on an app row → ask the WM
  to close that window via `_NET_CLOSE_WINDOW`. The app gets a chance to
  save state; the popup stays open and the row disappears.
- The first `d` arms the chord; any non-`d` keypress cancels it. **Super+D**
  remains the popup toggle and bypasses the chord entirely.

## Configuration

`scatto` looks for a TOML file at
`$XDG_CONFIG_HOME/scatto/config.toml` (fallback
`~/.config/scatto/config.toml`). If it's missing, the built-in defaults
below are used. Every key is optional — provide only the ones you want to
change.

To bootstrap a fresh config with all defaults written out as text:

```sh
scatto init-config
```

That creates `~/.config/scatto/` if needed and writes the file. It refuses
to overwrite an existing config — remove it manually if you want a clean
reset. To write the defaults somewhere else, pipe instead:

```sh
scatto print-config > /tmp/scatto.toml
```

Defaults:

```toml
[window]
width      = 520     # px
max_height = 640     # px; content scrolls if it would be taller
opacity    = 0.94    # 0.0 = fully transparent, 1.0 = opaque
                     # (visible transparency requires a compositor like picom)

[font]
family       = "Sans"   # any Pango font family
size         = 13.0     # absolute px
bold_headers = true     # bold weight on desktop headers
bold_apps    = true     # bold weight on app names (dim title stays regular)

[colors]
# Each value is hex: #RGB, #RGBA, #RRGGBB, or #RRGGBBAA. The alpha byte
# is honored everywhere except `background`, whose alpha is taken from
# `window.opacity` so transparency stays a single knob.
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
```

Save the file and reopen the popup — there's no live reload yet; the file
is read once per popup launch.

## Direct actions (no popup)

These subcommands act on `_NET_ACTIVE_WINDOW` directly so you can bind
them globally in your WM:

```
scatto switch-next   # next desktop
scatto switch-prev   # prev desktop
scatto move-next     # take active window to next desktop
scatto move-prev     # take active window to prev desktop
```

If you bind them to the same chord as the popup's internal keys, they
only fire when the popup is closed — once it's open, the keyboard grab
routes the chord to the popup instead.

## Build

Requires a Rust toolchain and the system libraries `libx11`/`libxcb`,
`cairo`, `pango`, and `pangocairo`. On Debian/Ubuntu:

```sh
sudo apt install libxcb1-dev libcairo2-dev libpango1.0-dev
make release
make install                       # → ~/.local/bin/scatto
```

## i3 / sxhkd example bindings

For i3 (`~/.config/i3/config`):

```
bindsym $mod+d         exec --no-startup-id scatto
bindsym $mod+Shift+j   exec --no-startup-id scatto switch-next
bindsym $mod+Shift+k   exec --no-startup-id scatto switch-prev
bindsym $mod+Control+j exec --no-startup-id scatto move-next
bindsym $mod+Control+k exec --no-startup-id scatto move-prev
```

For sxhkd (`~/.config/sxhkd/sxhkdrc`):

```
super + d
    scatto
super + shift + {j,k}
    scatto switch-{next,prev}
super + ctrl + {j,k}
    scatto move-{next,prev}
```

Inside the open popup, Super isn't required for any chord — the popup has
the keyboard grab, so `Shift+J`, `Shift+Ctrl+J`, `dd`, etc. all work
without holding Super.

## Requirements

- An X11 window manager that implements the EWMH bits used here:
  `_NET_NUMBER_OF_DESKTOPS`, `_NET_CURRENT_DESKTOP`, `_NET_DESKTOP_NAMES`
  (optional), `_NET_CLIENT_LIST` / `_NET_CLIENT_LIST_STACKING`,
  `_NET_ACTIVE_WINDOW`, `_NET_WM_DESKTOP`, `_NET_WM_NAME`, `_NET_WM_ICON`,
  `_NET_CLOSE_WINDOW`. Most modern WMs (i3, bspwm, openbox, xfwm, awesome,
  mutter on X11, kwin on X11) do.
- A compositor (e.g. picom) for transparency. Without one, the popup
  renders as solid dark.
