<p align="center"><img src="assets/logo/spitfire-wc-icon-192.png" width="96" alt="spitfire logo"></p>

<h1 align="center">spitfire</h1>

<p align="center">A tiling Wayland compositor for Linux, in the spirit of <a href="https://dwm.suckless.org/">dwm</a> —
one config file, reloadable without ever restarting your session.</p>

---

spitfire is a window manager/compositor for Wayland. If you've used **dwm**, the master-
stack tiling and the "one config file, no menus" philosophy will feel immediately
familiar — the difference is the config file is **Lua**, not something you recompile,
so changing a keybinding or a color takes a save and a keypress, not a rebuild.

It doesn't come bundled with a bar, launcher, or wallpaper app — you either use the
small bar built into spitfire itself, or point it at whatever you already like
(waybar, eww, your own Quickshell shell, ...). Either way, spitfire itself is just
the window manager.

## Features

- **Four layouts**, switchable per workspace at any time: `tile` (master-stack, like
  dwm), `floating`, `fibonacci` (spiral split), and `monocle` (one window at a time,
  full-screen).
- **One config file** (`~/.config/spitfire/config.lua`) — keybindings, colors, gaps,
  keyboard layout, autostart apps, all in one place, reloaded live with a keypress.
- **Dynamic workspaces** — there's no fixed number to set up in advance; asking to jump
  to workspace 7 just creates it.
- **Built-in status bar**, on by default: workspace list, active layout, CPU/RAM/
  battery/network, clock and date. No extra program to install or configure — turn
  it off with one line if you'd rather run your own (waybar, eww, ...).
- **Window borders and gaps**, colored however you like.
- **A lock screen that actually works** (`ext-session-lock-v1`) — compatible with
  `swaylock` and any other standard Wayland lock screen.
- **Runs your existing Linux apps**, X11 ones included — spitfire supports XWayland, so
  older/non-Wayland-native applications still work.
- Works as a real login-screen session (via `greetd` and similar) or nested inside
  another desktop session for trying it out first.

## Installing

There are no prebuilt packages yet — spitfire is built from source. You'll need a Rust
toolchain (get one at [rustup.rs](https://rustup.rs) if you don't have one already) and
a handful of development packages:

<details>
<summary><b>Void Linux</b></summary>

```sh
sudo xbps-install -S base-devel wayland-devel libxkbcommon-devel MesaLib-devel \
    libseat-devel libinput-devel libdisplay-info-devel
```
</details>

<details>
<summary><b>Arch Linux</b></summary>

```sh
sudo pacman -S base-devel wayland libxkbcommon mesa seatd libinput libdisplay-info
```
</details>

<details>
<summary><b>Debian / Ubuntu</b></summary>

```sh
sudo apt install build-essential libwayland-dev libxkbcommon-dev libegl1-mesa-dev \
    libgbm-dev libseat-dev libinput-dev libdisplay-info-dev
```
</details>

Then clone and install:

```sh
git clone https://github.com/dani-77/spitfire.git
cd spitfire
sudo make install
```

This builds spitfire in release mode and installs the `spitfire`/`spitfirectl` binaries,
an app icon, and a session entry so your login screen (greetd, GDM, SDDM, ...) can offer
**spitfire** as a login option, the same way it already offers GNOME, Hyprland, etc. Log
out, pick spitfire from the session list, log back in.

To try it out first without logging out — nested inside your current desktop session,
in a window:

```sh
cargo run -p spitfire -- --winit
```

To uninstall later: `sudo make uninstall`.

## Getting started

The first time spitfire runs, it looks for `~/.config/spitfire/config.lua` and, finding
nothing, starts with no keybindings at all — not even a way to open a terminal. Copy the
example config to get a sensible starting point:

```sh
mkdir -p ~/.config/spitfire
cp examples/config.lua ~/.config/spitfire/config.lua
```

Open it in an editor and change the one line that spawns a terminal (`alacritty` by
default — swap it for whatever terminal you actually have installed), then reload
spitfire (`Mod+Shift+R`, see below) or just log back in.

### Default keybindings

`Mod` is the Super/Windows key by default (`Mod4` in the config) — every single one of
these is just a line in `config.lua`, so rebind anything to anything.

| Keys | Action |
| --- | --- |
| `Mod` + `Return` | Open a terminal |
| `Mod` + `T` / `F` / `M` | Switch layout: tile / fibonacci (spiral) / monocle |
| `Mod` + `Shift` + `Space` | Switch to floating layout |
| `Mod` + `Space` | Cycle through layouts |
| `Mod` + `L` / `H` | Grow / shrink the master area |
| `Mod` + `I` / `D` | More / fewer windows in the master area |
| `Mod` + `1`-`9` | Switch to workspace 1-9 |
| `Alt` + `1`-`9` | Move the focused window to workspace 1-9 |
| `Mod` + `Q` | Close the focused window |
| `Mod` + `J` / `K` | Move keyboard focus to the next / previous window |
| `Mod` + `Shift` + `Q` | Quit spitfire (ends the session) |
| `Mod` + `Shift` + `R` | Reload `config.lua` — no logout needed |

## Configuring

Everything lives in `~/.config/spitfire/config.lua`. `examples/config.lua` in this repo
is both the default and a fully-commented reference — the easiest way to learn the
config is to read through it. A few of the things you can set:

```lua
-- Colors and spacing
spitfire.border = { width = 2, active = "#7aa2f7", inactive = "#414868" }
spitfire.gaps = { inner = 6, outer = 10 }

-- Keyboard layout (leave unset for the system default)
spitfire.keyboard = { layout = "de" }  -- or "pt", "fr", whatever setxkbmap would take

-- The built-in bar (on by default)
spitfire.bar = { enable = true, height = 28 }

-- Whatever you want started with the session — a bar, wallpaper, launcher, etc.
-- (skip this entirely if the built-in bar above is all you need)
spitfire.autostart({
  "swaybg -i ~/wallpaper.png",
})
```

Any change takes effect the moment you save the file and press `Mod+Shift+R` (or run
`spitfirectl reload` from a terminal) — no logout required.

## License

MIT — see [LICENSE](LICENSE). Third-party code this project builds on is credited in
[NOTICE.md](NOTICE.md).

Looking to build or contribute to spitfire itself, rather than just use it? See
[`doc/README.md`](doc/README.md) for the technical/architecture overview.
