//! The optional built-in bar (Phase 8, `spitfire.bar.enable` — off by
//! default). Unlike a swaybar/i3bar or Utumno's own `Bar.qml`, this isn't a
//! separate client process or protocol: it's drawn directly by the
//! compositor as a stack of solid-color rectangles, the exact same
//! `SolidColorRenderElement` primitive `spitfire.border` uses (see
//! `render::border_elements`) — chosen after that border-rendering effort
//! made clear how reliable that primitive is, and how much debugging a
//! real font/TTF renderer would have cost for comparatively little payoff
//! here.
//!
//! Content, left to right: workspace numbers (highlighted when active) +
//! a small icon for the active layout mode, then — right-aligned — CPU%,
//! RAM%, battery% (a `+` suffix while charging), network SSID + signal%,
//! and the clock/date. There is still no
//! real font/TTF: every glyph (digits, A-Z, and a handful of symbols) is
//! its own 5×7 bitmap built from rectangles (`glyph_for`), and the
//! layout-mode icon is a small geometric shape that echoes the actual
//! layout (`draw_layout_icon`) — same reasoning as before: a real font
//! renderer would have cost far more to get right than a bar has any
//! need for.
//!
//! The bar floats: it's inset from the top/left/right edges of the
//! output by `spitfire.gaps.outer`, the same outer gap windows already
//! get, rather than sitting flush against the screen edge.
//!
//! `spitfire::layout::TilingLayout::arrange` reserves `spitfire.bar.height`
//! plus that same gap (above *and* below the bar) at the top of the
//! output for this, the same way it already reserves layer-shell
//! exclusive zones — see `SpitfireState::arrange_tiling`.

use smithay::{
    backend::renderer::{
        element::{solid::SolidColorRenderElement, Element, Id, Kind, RenderElement},
        utils::CommitCounter,
        Color32F, Frame, ImportAll, ImportMem, Renderer,
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform},
};
use std::time::{Duration, Instant};

use spitfire_config::BarConfig;
use spitfire_layout::LayoutMode;

use crate::{
    render::{hex_to_color32f, CustomRenderElements, RectCache},
    state::{Backend, SpitfireState},
};

/// The bar's own runtime state: clock/date and CPU/RAM/network status
/// text, all refreshed at most once a second. Everything else it draws
/// (workspace list, active layout mode) is read straight from
/// `SpitfireState` every frame — cheap, and it means there's no second
/// copy of that state to fall out of sync.
#[derive(Debug, Default)]
pub struct Bar {
    last_tick: Option<Instant>,
    time_text: String,
    date_text: String,
    cpu_text: String,
    ram_text: String,
    bat_text: String,
    net_text: String,
    /// (total, idle) jiffies from `/proc/stat` as of the last tick — CPU%
    /// is a delta between two samples, not a point-in-time reading (the
    /// counters are cumulative since boot), so the first tick after
    /// startup only seeds this and `cpu_text` stays empty for one second.
    prev_cpu: Option<(u64, u64)>,
    /// Backing buffer for the bar's background strip, reused frame to
    /// frame so an unchanged bar produces zero damage instead of
    /// repainting at the full display refresh rate forever — see
    /// `RectCache`.
    rect_cache: RectCache,
    /// All `fg`-colored glyph rects (every workspace number but the active
    /// one, the layout icon, the status text) batched into one render
    /// element — see `GlyphBatch`.
    fg_batch: GlyphBatch,
    /// The active workspace's number, in `fg_active` — kept as its own
    /// batch since it's a different color from `fg_batch` (one
    /// `GlyphBatchElement` only ever draws in a single color).
    fg_active_batch: GlyphBatch,
}

impl Bar {
    /// Refreshes every piece of bar text, at most once a second. Checked
    /// every frame (cheap — one `Instant` comparison), but the actual
    /// work (spawning `date`, reading `/proc/stat`+`/proc/meminfo`,
    /// spawning `iw`) only happens on the second boundary, so this never
    /// stalls the render loop the way calling it unconditionally
    /// per-frame would.
    pub fn tick(&mut self) {
        let now = Instant::now();
        if self
            .last_tick
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        self.last_tick = Some(now);

        // Shelling out to `date` (rather than a datetime crate) is
        // deliberate: it picks up the system timezone/locale for free.
        if let Ok(output) = std::process::Command::new("date")
            .arg("+%H:%M %d.%m.%Y")
            .output()
        {
            if let Ok(text) = String::from_utf8(output.stdout) {
                if let Some((time, date)) = text.trim().split_once(' ') {
                    self.time_text = time.to_string();
                    self.date_text = date.to_string();
                }
            }
        }

        if let Some((total, idle)) = read_cpu_total_idle() {
            if let Some((prev_total, prev_idle)) = self.prev_cpu.replace((total, idle)) {
                let total_delta = total.saturating_sub(prev_total);
                let idle_delta = idle.saturating_sub(prev_idle);
                if let Some(usage) = idle_delta.saturating_mul(100).checked_div(total_delta) {
                    self.cpu_text = format!("{}%", 100u64.saturating_sub(usage));
                }
            }
        }

        if let Some(percent) = read_ram_percent() {
            self.ram_text = format!("{percent}%");
        }

        self.bat_text = read_battery_status()
            .map(|(percent, charging)| {
                if charging {
                    format!("{percent}%+")
                } else {
                    format!("{percent}%")
                }
            })
            .unwrap_or_else(|| "--".to_string());

        self.net_text = read_network_status()
            .map(|(ssid, percent)| format!("{ssid} {percent}%"))
            .unwrap_or_else(|| "--".to_string());
    }

    /// The whole right-hand side of the bar as one string, in the fixed
    /// order requested: `CPU: n% | RAM: n% | BAT: n% | NETWORK: ssid n% |
    /// HH:MM - DD.MM.YYYY`. Uppercased because the bitmap font below only
    /// has glyphs for A-Z — an SSID with lowercase letters still shows up
    /// (just not case-accurately), rather than silently vanishing.
    pub fn status_text(&self) -> String {
        format!(
            "CPU: {} | RAM: {} | BAT: {} | NETWORK: {} | {} - {}",
            self.cpu_text,
            self.ram_text,
            self.bat_text,
            self.net_text,
            self.time_text,
            self.date_text
        )
        .to_uppercase()
    }
}

/// Total and idle jiffies from `/proc/stat`'s first line (`cpu  user nice
/// system idle iowait ...`) — idle *and* iowait count as "not busy", the
/// same convention `top`/`htop` use.
fn read_cpu_total_idle() -> Option<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/stat").ok()?;
    let line = content.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let nums: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    let total: u64 = nums.iter().sum();
    Some((total, idle))
}

/// Percentage of RAM in use, from `/proc/meminfo`'s `MemTotal`/
/// `MemAvailable` (the latter already accounts for reclaimable
/// cache/buffers, unlike the older `MemFree` — same figure `free -h`
/// bases "available" on).
fn read_ram_percent() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next()?.parse::<u64>().ok();
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    let (total, available) = (total?, available?);
    if total == 0 {
        return None;
    }
    Some(total.saturating_sub(available) * 100 / total)
}

/// Battery percentage and whether it's currently charging, from the first
/// `/sys/class/power_supply/BAT*` entry found — `None` on a desktop (no
/// `BAT*` directory at all), which shows "--" the same way a wireless-less
/// machine does for the network segment.
fn read_battery_status() -> Option<(u8, bool)> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("BAT") {
            continue;
        }
        let path = entry.path();
        let Ok(capacity) = std::fs::read_to_string(path.join("capacity")) else {
            continue;
        };
        let Ok(percent) = capacity.trim().parse::<u8>() else {
            continue;
        };
        let charging = std::fs::read_to_string(path.join("status"))
            .is_ok_and(|s| s.trim().eq_ignore_ascii_case("charging"));
        return Some((percent, charging));
    }
    None
}

/// The active wireless connection's SSID and signal strength, via `iw`
/// (present on essentially every Linux system with wireless hardware,
/// unlike e.g. NetworkManager/`nmcli`, which not everyone runs). `None`
/// if there's no wireless interface, it's not associated, or `iw` isn't
/// installed — any of which just shows "--" instead of a name/percent.
fn read_network_status() -> Option<(String, u8)> {
    let dev_output = std::process::Command::new("iw").arg("dev").output().ok()?;
    let dev_text = String::from_utf8(dev_output.stdout).ok()?;
    let iface = dev_text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Interface "))?
        .to_string();

    let link_output = std::process::Command::new("iw")
        .args(["dev", &iface, "link"])
        .output()
        .ok()?;
    let link_text = String::from_utf8(link_output.stdout).ok()?;
    if link_text.trim_start().starts_with("Not connected") {
        return None;
    }

    let ssid = link_text
        .lines()
        .find_map(|l| l.trim().strip_prefix("SSID: "))?
        .to_string();

    // dBm→quality heuristic: -50 dBm or better is "100%", -100 dBm or
    // worse is "0%" — the same one NetworkManager/wpa_supplicant
    // frontends use, so the number lines up with what users already
    // expect from other tools.
    let percent = link_text
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("signal: ")
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<i32>().ok())
        })
        .map(|dbm| (2 * (dbm + 100)).clamp(0, 100) as u8)
        .unwrap_or(0);

    Some((ssid, percent))
}

/// One workspace as the bar needs it: its (1-based) number and whether
/// it's the currently active one.
pub struct BarWorkspaceItem {
    pub number: usize,
    pub active: bool,
}

/// Everything read from compositor state each frame to draw the left side
/// of the bar — computed by `SpitfireState::bar_data`, turned into render
/// elements by `bar_elements` (kept separate since the latter is generic
/// over the renderer and this data isn't).
pub struct BarData {
    pub workspaces: Vec<BarWorkspaceItem>,
    pub mode: LayoutMode,
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    /// The workspace list + active layout mode the bar's left side shows.
    /// v1 scope: a single `WorkspaceSet` (see `crate::workspace`), so this
    /// is simply every workspace that exists, in order.
    pub fn bar_data(&self) -> BarData {
        let active_index = self.workspaces.active_index();
        let workspaces = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(i, _)| BarWorkspaceItem {
                number: i + 1,
                active: i == active_index,
            })
            .collect();
        BarData {
            workspaces,
            mode: self.workspaces.active().tiling.params.mode,
        }
    }
}

// --- geometry helpers, all in logical pixels ------------------------------

/// One glyph's pixels, 5 wide × 7 tall — bit 4 of each row is the
/// leftmost column, bit 0 the rightmost. Own bespoke design (not lifted
/// from any existing font file), in the same blocky/rectangular spirit as
/// the layout-mode icons below: covers `0`-`9`, `A`-`Z`, and the symbols
/// the bar actually needs (`%`, `-`/`_`, `:`, `.`, `|`). Anything else
/// (accented letters, punctuation an SSID might use, ...) just doesn't
/// draw — `glyph_advance` skips it with zero width rather than guessing.
type Glyph = [u8; 7];

const GLYPH_COLS: i32 = 5;
const GLYPH_ROWS: i32 = 7;

fn glyph_for(c: char) -> Option<Glyph> {
    Some(match c {
        '0' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x0E, 0x11, 0x01, 0x06, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x0E, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x11, 0x0E],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0E],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x11, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0E, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x0E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '%' => [0x18, 0x18, 0x02, 0x04, 0x08, 0x03, 0x03],
        '+' => [0x04, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x04],
        '-' | '_' => [0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x00],
        ':' => [0x00, 0x04, 0x00, 0x00, 0x04, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        _ => return None,
    })
}

/// Side length in logical pixels of one glyph "pixel" for a given bar
/// glyph height — everything else (advance width, per-pixel rect size)
/// is derived from this.
fn glyph_pixel_size(height: i32) -> i32 {
    (height / GLYPH_ROWS).max(1)
}

/// Advance width (glyph + no trailing gap) of one character — a known
/// glyph is `GLYPH_COLS` pixels wide, a space is half that, anything
/// unrecognized (see `glyph_for`) contributes nothing.
fn glyph_advance(c: char, height: i32) -> i32 {
    let px = glyph_pixel_size(height);
    if c == ' ' {
        (GLYPH_COLS * px) / 2
    } else if glyph_for(c).is_some() {
        GLYPH_COLS * px
    } else {
        0
    }
}

/// Total width `draw_text` will take up for `s` at the given glyph
/// height and inter-glyph gap — used to right-align the status text.
fn measure_text(s: &str, height: i32, gap: i32) -> i32 {
    let mut w: i32 = s.chars().map(|c| glyph_advance(c, height) + gap).sum();
    if w > 0 {
        w -= gap;
    }
    w
}

fn push_rect<R>(
    elements: &mut Vec<CustomRenderElements<R>>,
    cache: &mut RectCache,
    rect: Rectangle<i32, Logical>,
    color: Color32F,
    output_scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
{
    if rect.size.w <= 0 || rect.size.h <= 0 {
        return;
    }
    let buffer = cache.next(rect.size, color);
    let loc: Point<i32, Physical> = rect.loc.to_f64().to_physical(output_scale).to_i32_round();
    elements.push(CustomRenderElements::Solid(
        SolidColorRenderElement::from_buffer(buffer, loc, output_scale, 1.0, Kind::Unspecified),
    ));
}

/// Converts one logical-space rect into physical space and appends it to
/// `rects` for later batching into one `GlyphBatchElement` — the glyph
/// counterpart of `push_rect`, which instead emits a whole separate
/// `SolidColorRenderElement` per rect (fine for the handful of border
/// strips it's built for, ruinous for the hundreds of tiny rects a status
/// line's worth of bitmap-font text needs; see `GlyphBatch`'s docs).
fn push_phys_rect(
    rects: &mut Vec<Rectangle<i32, Physical>>,
    rect: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) {
    if rect.size.w <= 0 || rect.size.h <= 0 {
        return;
    }
    rects.push(rect.to_physical_precise_round(output_scale));
}

/// One (color, list-of-rects) group of the bar's bitmap-font glyphs,
/// bundled into a single render element instead of the many individual
/// ones `push_rect` would otherwise emit — see `GlyphBatch`'s docs for why.
#[derive(Debug, Clone)]
pub(crate) struct GlyphBatchElement {
    id: Id,
    geometry: Rectangle<i32, Physical>,
    rects: Vec<Rectangle<i32, Physical>>,
    color: Color32F,
    commit: CommitCounter,
}

impl Element for GlyphBatchElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        // Unused by `draw` below (solid colors have no source buffer to
        // sample from) — same reasoning, and same computation, as
        // `SolidColorRenderElement::new`'s own `src`.
        let size = self.geometry.size.to_f64().to_logical(1f64);
        Rectangle::from_size(size).to_buffer(1f64, Transform::Normal, &size)
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    // `opaque_regions` is intentionally left at its default (empty): the
    // bar's background strip, drawn separately right behind this, already
    // covers this element's whole area, so there's nothing this element
    // needs to contribute to occlusion — and *not* contributing it is the
    // entire point, see `GlyphBatch`'s docs.
}

impl<R: Renderer> RenderElement<R> for GlyphBatchElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        // `draw` only runs when the compositor already decided this
        // element needs (re)drawing (see `Element::damage_since`'s
        // default, which we rely on: whole-geometry damage or none), so
        // every sub-rect gets fully redrawn — no need to intersect against
        // `_damage`. Crucially, `draw_solid`'s damage argument is read
        // *relative to* the `dst` rect it's given (0,0 = that rect's own
        // corner — see its GLES implementation, which clamps into
        // `[0, dst.size]`), not in absolute output coordinates like
        // `self.rects` are; passing `_damage` through unadjusted here was
        // the bug that first made this draw nothing at all.
        for &rect in &self.rects {
            frame.draw_solid(rect, &[Rectangle::from_size(rect.size)], self.color)?;
        }
        Ok(())
    }
}

/// Persistent identity + content for one `GlyphBatchElement`: keeps the
/// same `Id`/`CommitCounter` across frames when given the same rects/color
/// as last time — same reasoning as `RectCache`, just at the scale of a
/// whole batch of rects instead of one each.
///
/// This exists because a `SolidColorRenderElement` per lit bitmap pixel —
/// what `push_rect`-based drawing used to do — turned out to cost far more
/// than the GPU work of drawing them: smithay's damage tracker subtracts
/// every *opaque* element's region from every other one behind it
/// (`Rectangle::subtract_rects_many_in_place`), an algorithm whose cost
/// scales with the number of such elements, and a status line's worth of
/// individually-opaque glyph pixels runs into the hundreds. Measured with
/// `perf`: over 80% of *all* sampled CPU cycles, on every frame the bar
/// was composited, damage or no damage — not something `RectCache` alone
/// (which keeps *unchanged* rects from re-triggering that work, but does
/// nothing about how many rects a *changed* frame still has to feed it)
/// could fix. Batching every rect of one color into a single element with
/// no `opaque_regions` of its own (see `GlyphBatchElement`) sidesteps the
/// algorithm entirely: from its perspective the bar's text is one thing,
/// not hundreds.
#[derive(Debug, Default)]
struct GlyphBatch {
    id: Option<Id>,
    rects: Vec<Rectangle<i32, Physical>>,
    color: Color32F,
    commit: CommitCounter,
}

impl GlyphBatch {
    /// Updates this batch's content, bumping the commit counter only if
    /// it actually differs from what was drawn last frame, and returns the
    /// render element for this frame — `None` if `rects` is empty (nothing
    /// to draw, e.g. an empty status text).
    fn element(
        &mut self,
        rects: Vec<Rectangle<i32, Physical>>,
        color: Color32F,
    ) -> Option<GlyphBatchElement> {
        if rects.is_empty() {
            return None;
        }
        let id = self.id.get_or_insert_with(Id::new).clone();
        if self.rects != rects || self.color != color {
            self.rects = rects;
            self.color = color;
            self.commit.increment();
        }
        let geometry = self
            .rects
            .iter()
            .copied()
            .reduce(Rectangle::merge)
            .expect("checked non-empty above");
        Some(GlyphBatchElement {
            id,
            geometry,
            rects: self.rects.clone(),
            color: self.color,
            commit: self.commit,
        })
    }
}

/// Decodes one glyph into a minimal set of `(col, row, width, height)`
/// rects in glyph-local pixel coordinates, instead of one per lit pixel:
/// horizontally-contiguous lit runs within a row become one rect, then
/// vertically-adjacent rows with the *identical* run pattern (true for
/// most of a glyph's height — e.g. a letter's uninterrupted side strokes)
/// merge into one taller rect.
///
/// This matters far more than it looks: every rect here becomes a fully
/// opaque `SolidColorRenderElement`, and smithay's damage tracker has to
/// subtract every opaque element's region from every other one behind it
/// (`Rectangle::subtract_rects_many_in_place`) — quadratic in the number
/// of elements. A whole status line of one-rect-per-pixel glyphs runs into
/// the hundreds of elements, which made that subtraction the dominant CPU
/// cost on *every* frame the bar is composited, not just when its text
/// changes. Merging cuts the count per glyph by roughly 3-4x, which — being
/// squared away in a quadratic algorithm — cuts that cost by closer to an
/// order of magnitude.
fn glyph_rects(glyph: &Glyph) -> Vec<(i32, i32, i32, i32)> {
    let row_intervals: Vec<Vec<(i32, i32)>> = glyph
        .iter()
        .map(|&bits| {
            let mut intervals = Vec::new();
            let mut col = 0i32;
            while col < GLYPH_COLS {
                let lit = |c: i32| bits & (1 << (GLYPH_COLS - 1 - c) as u32) != 0;
                if lit(col) {
                    let start = col;
                    while col < GLYPH_COLS && lit(col) {
                        col += 1;
                    }
                    intervals.push((start, col - start));
                } else {
                    col += 1;
                }
            }
            intervals
        })
        .collect();

    let mut rects = Vec::new();
    let mut row = 0usize;
    while row < row_intervals.len() {
        if row_intervals[row].is_empty() {
            row += 1;
            continue;
        }
        let mut height = 1;
        while row + height < row_intervals.len()
            && row_intervals[row + height] == row_intervals[row]
        {
            height += 1;
        }
        rects.extend(
            row_intervals[row]
                .iter()
                .map(|&(col, w)| (col, row as i32, w, height as i32)),
        );
        row += height;
    }
    rects
}

/// Draws one text string left-to-right starting at `(x, y)`, appending the
/// merged rects `glyph_rects` computes for each character to `rects` (all
/// drawn in the one `color` this call gets — see `GlyphBatch`, which is
/// why `draw_text` no longer builds render elements directly), and
/// returning the x position right after the last glyph (unknown characters
/// are skipped entirely — see `glyph_for`).
#[allow(clippy::too_many_arguments)]
fn draw_text(
    rects: &mut Vec<Rectangle<i32, Physical>>,
    text: &str,
    mut x: i32,
    y: i32,
    height: i32,
    gap: i32,
    output_scale: Scale<f64>,
) -> i32 {
    let px = glyph_pixel_size(height);
    for c in text.chars() {
        if let Some(glyph) = glyph_for(c) {
            for (col, row, w, h) in glyph_rects(&glyph) {
                push_phys_rect(
                    rects,
                    Rectangle::new((x + col * px, y + row * px).into(), (w * px, h * px).into()),
                    output_scale,
                );
            }
        }
        x += glyph_advance(c, height) + gap;
    }
    x
}

/// A small geometric icon standing in for a font glyph, echoing the shape
/// of the actual layout — drawn in a `size` × `size` box anchored at
/// `(x, y)`.
fn draw_layout_icon(
    rects: &mut Vec<Rectangle<i32, Physical>>,
    mode: LayoutMode,
    x: i32,
    y: i32,
    size: i32,
    output_scale: Scale<f64>,
) {
    let t = (size / 8).max(1);
    match mode {
        LayoutMode::Tile => {
            // Master column on the left, two stacked windows on the right —
            // dwm's master-stack, in miniature.
            let master_w = (size * 3) / 5;
            let stack_w = (size - master_w - t).max(1);
            let stack_h = ((size - t) / 2).max(1);
            push_phys_rect(
                rects,
                Rectangle::new((x, y).into(), (master_w, size).into()),
                output_scale,
            );
            push_phys_rect(
                rects,
                Rectangle::new((x + master_w + t, y).into(), (stack_w, stack_h).into()),
                output_scale,
            );
            push_phys_rect(
                rects,
                Rectangle::new(
                    (x + master_w + t, y + stack_h + t).into(),
                    (stack_w, stack_h).into(),
                ),
                output_scale,
            );
        }
        LayoutMode::Monocle => {
            push_phys_rect(
                rects,
                Rectangle::new((x, y).into(), (size, size).into()),
                output_scale,
            );
        }
        LayoutMode::Floating => {
            // A hollow square outline — four thin strips, the same trick
            // `spitfire.border` uses so it never has to rely on occlusion
            // to punch a hole through anything.
            push_phys_rect(
                rects,
                Rectangle::new((x, y).into(), (size, t).into()),
                output_scale,
            );
            push_phys_rect(
                rects,
                Rectangle::new((x, y + size - t).into(), (size, t).into()),
                output_scale,
            );
            push_phys_rect(
                rects,
                Rectangle::new((x, y).into(), (t, size).into()),
                output_scale,
            );
            push_phys_rect(
                rects,
                Rectangle::new((x + size - t, y).into(), (t, size).into()),
                output_scale,
            );
        }
        LayoutMode::Fibonacci => {
            // Top+left edges of three shrinking, nested boxes — a hint of
            // the spiral split without drawing the whole thing.
            let (mut rx, mut ry, mut rw, mut rh) = (x, y, size, size);
            for _ in 0..3 {
                push_phys_rect(
                    rects,
                    Rectangle::new((rx, ry).into(), (rw.max(t), t).into()),
                    output_scale,
                );
                push_phys_rect(
                    rects,
                    Rectangle::new((rx, ry).into(), (t, rh.max(t)).into()),
                    output_scale,
                );
                rx += rw / 2;
                ry += rh / 2;
                rw -= rw / 2;
                rh -= rh / 2;
            }
        }
    }
}

/// Builds every render element for one frame of the bar: the background
/// strip, workspace list + layout-mode icon on the left, CPU/RAM/network/
/// clock/date on the right. Returns nothing if `config.enabled` is
/// `false` — callers can call this unconditionally.
///
/// `outer_gap` insets the whole bar from the top/left/right edges of the
/// output — `spitfire.gaps.outer`, the same gap windows get, so the bar
/// floats rather than sitting flush against the screen edge. Square
/// corners throughout: every glyph, icon and the background itself is
/// built from `SolidColorRenderElement` rects, which have no other kind
/// of corner to give.
#[allow(clippy::too_many_arguments)]
pub fn bar_elements<R>(
    config: &BarConfig,
    output_width: i32,
    outer_gap: i32,
    data: &BarData,
    status_text: &str,
    output_scale: Scale<f64>,
    bar: &mut Bar,
) -> Vec<CustomRenderElements<R>>
where
    R: Renderer + ImportAll + ImportMem,
{
    let mut elements = Vec::new();
    if !config.enabled || config.height <= 0 {
        bar.rect_cache.finish_frame();
        return elements;
    }

    let height = config.height;
    let bg = hex_to_color32f(config.bg);
    let fg = hex_to_color32f(config.fg);
    let fg_active = hex_to_color32f(config.fg_active);

    let outer_gap = outer_gap.max(0);
    let bar_width = (output_width - outer_gap * 2).max(0);

    let pad = (height / 4).max(1);
    let glyph_h = (height - pad * 2).max(1);
    let gap = (glyph_h / 6).max(1);

    // Every glyph rect gets bucketed by color rather than turned into its
    // own render element right away — `fg_batch`/`fg_active_batch` (see
    // `GlyphBatch`) fold each bucket into a single element afterwards, so
    // the damage tracker sees two things here (three with the background),
    // never one per lit pixel.
    let mut fg_rects = Vec::new();
    let mut fg_active_rects = Vec::new();

    // Left: workspace numbers (highlighted when active) + layout-mode icon.
    let mut x = outer_gap + pad;
    let content_y = outer_gap + pad;
    for item in &data.workspaces {
        let rects = if item.active {
            &mut fg_active_rects
        } else {
            &mut fg_rects
        };
        x = draw_text(
            rects,
            &item.number.to_string(),
            x,
            content_y,
            glyph_h,
            gap,
            output_scale,
        );
        x += pad;
    }
    draw_layout_icon(
        &mut fg_rects,
        data.mode,
        x,
        content_y,
        glyph_h,
        output_scale,
    );

    // Right: CPU/RAM/NETWORK/clock/date, right-aligned.
    let text_w = measure_text(status_text, glyph_h, gap);
    let start_x = (outer_gap + bar_width - pad - text_w).max(x);
    draw_text(
        &mut fg_rects,
        status_text,
        start_x,
        content_y,
        glyph_h,
        gap,
        output_scale,
    );

    if let Some(e) = bar.fg_batch.element(fg_rects, fg) {
        elements.push(CustomRenderElements::GlyphBatch(e));
    }
    if let Some(e) = bar.fg_active_batch.element(fg_active_rects, fg_active) {
        elements.push(CustomRenderElements::GlyphBatch(e));
    }

    // Background strip, inset by outer_gap on the top/left/right so the
    // bar floats — pushed last, not first: this element list is
    // painter's-algorithm ordered with index 0 on top (the same
    // convention the cursor relies on to stay above windows in
    // `winit.rs`), so an opaque strip pushed *before* the digits/icon
    // above it would just paint over them. Same lesson as
    // `spitfire.border`'s render-order bug, just the opposite end of the
    // list this time.
    push_rect(
        &mut elements,
        &mut bar.rect_cache,
        Rectangle::new((outer_gap, outer_gap).into(), (bar_width, height).into()),
        bg,
        output_scale,
    );

    bar.rect_cache.finish_frame();

    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `glyph_rects`' merged rects must cover exactly the same pixels as
    /// the naive one-rect-per-lit-bit reading of the glyph — no more (that
    /// would draw pixels the font never lit), no less (missing pixels),
    /// and no overlaps (wasted, overlapping opaque elements defeats the
    /// point of merging them in the first place).
    fn lit_pixels(glyph: &Glyph) -> std::collections::HashSet<(i32, i32)> {
        let mut pixels = std::collections::HashSet::new();
        for (row, &bits) in glyph.iter().enumerate() {
            for col in 0..GLYPH_COLS {
                if bits & (1 << (GLYPH_COLS - 1 - col) as u32) != 0 {
                    pixels.insert((col, row as i32));
                }
            }
        }
        pixels
    }

    fn rects_pixels(rects: &[(i32, i32, i32, i32)]) -> Vec<(i32, i32)> {
        rects
            .iter()
            .flat_map(|&(col, row, w, h)| {
                (col..col + w).flat_map(move |c| (row..row + h).map(move |r| (c, r)))
            })
            .collect()
    }

    #[test]
    fn merged_rects_cover_exactly_the_lit_pixels_for_every_glyph() {
        // '0'-'9', 'A'-'Z', and the handful of symbols glyph_for knows —
        // every character that can actually reach draw_text.
        let chars = ('0'..='9')
            .chain('A'..='Z')
            .chain(['%', '+', '-', '_', ':', '.', '|']);
        for c in chars {
            let glyph = glyph_for(c).unwrap_or_else(|| panic!("no glyph for {c:?}"));
            let rects = glyph_rects(&glyph);
            let pixels = rects_pixels(&rects);

            let mut seen = std::collections::HashSet::new();
            for p in &pixels {
                assert!(
                    seen.insert(*p),
                    "{c:?}: pixel {p:?} covered by overlapping rects"
                );
            }
            assert_eq!(
                seen,
                lit_pixels(&glyph),
                "{c:?}: merged rects don't cover the same pixels as the glyph"
            );
        }
    }

    #[test]
    fn merging_meaningfully_reduces_rect_count() {
        // '0': solid top/bottom bars plus two untouched side strokes for
        // 5 rows — should merge from 16 lit pixels down to 4 rects.
        let glyph = glyph_for('0').unwrap();
        assert_eq!(glyph_rects(&glyph).len(), 4);
    }

    fn phys_rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// The entire point of `GlyphBatch`: unchanged content keeps the same
    /// `Id` *and* the same `CommitCounter` across calls, so the damage
    /// tracker reads it as "nothing changed here" — see the module-level
    /// docs on `GlyphBatch` for why that's the whole fix.
    #[test]
    fn unchanged_content_keeps_the_same_id_and_commit() {
        let mut batch = GlyphBatch::default();
        let rects = vec![phys_rect(0, 0, 5, 7), phys_rect(10, 0, 5, 7)];
        let color = Color32F::new(1.0, 1.0, 1.0, 1.0);

        let first = batch.element(rects.clone(), color).unwrap();
        let second = batch.element(rects, color).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(first.commit, second.commit);
    }

    /// Changed content (different rects, or a different color) keeps the
    /// same `Id` — it's still logically "the fg batch" — but bumps the
    /// commit, so the tracker knows to actually redraw it.
    #[test]
    fn changed_content_keeps_the_id_but_bumps_the_commit() {
        let mut batch = GlyphBatch::default();
        let color = Color32F::new(1.0, 1.0, 1.0, 1.0);
        let first = batch.element(vec![phys_rect(0, 0, 5, 7)], color).unwrap();
        let second = batch.element(vec![phys_rect(0, 0, 5, 8)], color).unwrap();

        assert_eq!(first.id, second.id);
        assert_ne!(first.commit, second.commit);
    }

    #[test]
    fn empty_rects_produce_no_element() {
        let mut batch = GlyphBatch::default();
        assert!(batch
            .element(Vec::new(), Color32F::new(1.0, 1.0, 1.0, 1.0))
            .is_none());
    }

    /// The whole reason `GlyphBatch` exists: a realistic status line's
    /// worth of text collapses into a small, constant number of render
    /// elements (2 glyph batches + 1 background rect) instead of one per
    /// lit bitmap pixel (hundreds — see `GlyphBatch`'s docs for why that
    /// was so expensive). Exercises the same `draw_text`/`draw_layout_icon`
    /// path `bar_elements` uses, just without needing a real renderer.
    #[test]
    fn a_realistic_bar_collapses_to_a_handful_of_glyph_batches() {
        let status =
            "CPU: 66% | RAM: 45% | BAT: 82%+ | NETWORK: MYHOMEWIFI 80% | 14:23 - 06.08.2026"
                .to_uppercase();
        let scale = Scale::from(1.0);

        let mut fg_rects = Vec::new();
        let mut x = 0;
        for n in 1..=5 {
            x = draw_text(&mut fg_rects, &n.to_string(), x, 0, 21, 3, scale);
            x += 4;
        }
        draw_layout_icon(&mut fg_rects, LayoutMode::Tile, x, 0, 21, scale);
        draw_text(&mut fg_rects, &status, x, 0, 21, 3, scale);

        // Hundreds of individual glyph rects, as expected...
        assert!(
            fg_rects.len() > 200,
            "expected a few hundred glyph rects, got {}",
            fg_rects.len()
        );

        // ...but batched into exactly one render element, not one each.
        let mut batch = GlyphBatch::default();
        let color = Color32F::new(1.0, 1.0, 1.0, 1.0);
        assert!(batch.element(fg_rects, color).is_some());
    }
}
