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
//! a small icon for the active layout mode, then — right-aligned — a
//! clock and date. There is no font: digits are 7-segment glyphs built
//! from rectangles (`draw_digit`), and the layout-mode icon is a small
//! geometric shape that echoes the actual layout (`draw_layout_icon`).
//!
//! `spitfire::layout::TilingLayout::arrange` reserves `spitfire.bar.height`
//! at the top of the output for this, the same way it already reserves
//! layer-shell exclusive zones — see `SpitfireState::arrange_tiling`.

use smithay::{
    backend::renderer::{
        element::{solid::SolidColorBuffer, solid::SolidColorRenderElement, Kind},
        Color32F, ImportAll, ImportMem, Renderer,
    },
    utils::{Logical, Physical, Point, Rectangle, Scale},
};
use std::time::{Duration, Instant};

use spitfire_config::BarConfig;
use spitfire_layout::LayoutMode;

use crate::{
    render::{hex_to_color32f, CustomRenderElements},
    state::{Backend, SpitfireState},
};

/// The bar's own runtime state: just the clock/date text, refreshed at
/// most once a second. Everything else it draws (workspace list, active
/// layout mode) is read straight from `SpitfireState` every frame — cheap,
/// and it means there's no second copy of that state to fall out of sync.
#[derive(Debug, Default)]
pub struct Bar {
    last_tick: Option<Instant>,
    time_text: String,
    date_text: String,
}

impl Bar {
    /// Refreshes `time_text`/`date_text` by shelling out to `date`, at most
    /// once a second. Checked every frame (cheap — one `Instant`
    /// comparison), but the actual process spawn only happens on the
    /// second boundary, so this never stalls the render loop the way
    /// calling it unconditionally per-frame would. Shelling out (rather
    /// than a datetime crate) is deliberate: it picks up the system
    /// timezone/locale for free.
    pub fn tick(&mut self) {
        let now = Instant::now();
        if self
            .last_tick
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
        {
            return;
        }
        self.last_tick = Some(now);

        let Ok(output) = std::process::Command::new("date")
            .arg("+%H:%M %d.%m")
            .output()
        else {
            return;
        };
        let Ok(text) = String::from_utf8(output.stdout) else {
            return;
        };
        if let Some((time, date)) = text.trim().split_once(' ') {
            self.time_text = time.to_string();
            self.date_text = date.to_string();
        }
    }

    pub fn time_text(&self) -> &str {
        &self.time_text
    }

    pub fn date_text(&self) -> &str {
        &self.date_text
    }
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

/// A 7-segment digit's bounding box width for a given height — a fixed
/// aspect ratio, same as any digital-clock display.
fn digit_width(height: i32) -> i32 {
    ((height as f64) * 0.55).round() as i32
}

/// Segment thickness for a given digit height.
fn digit_thickness(height: i32) -> i32 {
    (((height as f64) * 0.16).round() as i32).max(1)
}

/// Segment lit/unlit table for digits 0-9, in `[a, b, c, d, e, f, g]`
/// order (`a` = top, going clockwise, `g` = middle) — the standard
/// unambiguous 7-segment encoding.
const SEGMENTS_BY_DIGIT: [[bool; 7]; 10] = [
    [true, true, true, true, true, true, false],     // 0
    [false, true, true, false, false, false, false], // 1
    [true, true, false, true, true, false, true],    // 2
    [true, true, true, true, false, false, true],    // 3
    [false, true, true, false, false, true, true],   // 4
    [true, false, true, true, false, true, true],    // 5
    [true, false, true, true, true, true, true],     // 6
    [true, true, true, false, false, false, false],  // 7
    [true, true, true, true, true, true, true],      // 8
    [true, true, true, true, false, true, true],     // 9
];

/// The 7 segment rectangles for a digit box of the given height, anchored
/// at `(x, y)` (top-left corner) in logical pixels.
fn digit_segments(x: i32, y: i32, height: i32) -> [Rectangle<i32, Logical>; 7] {
    let w = digit_width(height);
    let t = digit_thickness(height);
    let half = height / 2;
    [
        // a: top
        Rectangle::new((x + t, y).into(), ((w - 2 * t).max(0), t).into()),
        // b: top-right
        Rectangle::new((x + w - t, y + t).into(), (t, (half - t).max(0)).into()),
        // c: bottom-right
        Rectangle::new(
            (x + w - t, y + half).into(),
            (t, (height - t - half).max(0)).into(),
        ),
        // d: bottom
        Rectangle::new(
            (x + t, y + height - t).into(),
            ((w - 2 * t).max(0), t).into(),
        ),
        // e: bottom-left
        Rectangle::new((x, y + half).into(), (t, (height - t - half).max(0)).into()),
        // f: top-left
        Rectangle::new((x, y + t).into(), (t, (half - t).max(0)).into()),
        // g: middle
        Rectangle::new(
            (x + t, y + half - t / 2).into(),
            ((w - 2 * t).max(0), t).into(),
        ),
    ]
}

/// Advance width (glyph + no trailing gap) of one character in a bar text
/// string — digits, `:`, `.`, and plain spacing between digit groups.
fn glyph_width(c: char, height: i32) -> i32 {
    match c {
        '0'..='9' => digit_width(height),
        ':' | '.' => (digit_thickness(height) * 2).max(2),
        ' ' => digit_width(height) / 2,
        _ => 0,
    }
}

/// Total width `draw_text` will take up for `s` at the given digit height
/// and inter-glyph gap — used to right-align the clock/date.
fn measure_text(s: &str, height: i32, gap: i32) -> i32 {
    let mut w: i32 = s.chars().map(|c| glyph_width(c, height) + gap).sum();
    if w > 0 {
        w -= gap;
    }
    w
}

fn push_rect<R>(
    elements: &mut Vec<CustomRenderElements<R>>,
    rect: Rectangle<i32, Logical>,
    color: Color32F,
    output_scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
{
    if rect.size.w <= 0 || rect.size.h <= 0 {
        return;
    }
    let buffer = SolidColorBuffer::new(rect.size, color);
    let loc: Point<i32, Physical> = rect.loc.to_f64().to_physical(output_scale).to_i32_round();
    elements.push(CustomRenderElements::Solid(
        SolidColorRenderElement::from_buffer(&buffer, loc, output_scale, 1.0, Kind::Unspecified),
    ));
}

/// Draws one text string (digits, `:`, `.`, spaces) left-to-right starting
/// at `(x, y)`, returning the x position right after the last glyph.
#[allow(clippy::too_many_arguments)]
fn draw_text<R>(
    elements: &mut Vec<CustomRenderElements<R>>,
    text: &str,
    mut x: i32,
    y: i32,
    height: i32,
    gap: i32,
    color: Color32F,
    output_scale: Scale<f64>,
) -> i32
where
    R: Renderer + ImportAll + ImportMem,
{
    for c in text.chars() {
        match c {
            '0'..='9' => {
                let digit = c as u8 - b'0';
                let on = SEGMENTS_BY_DIGIT[digit as usize];
                for (rect, lit) in digit_segments(x, y, height).into_iter().zip(on) {
                    if lit {
                        push_rect(elements, rect, color, output_scale);
                    }
                }
            }
            ':' => {
                let s = digit_thickness(height).max(1) * 2;
                push_rect(
                    elements,
                    Rectangle::new((x, y + height / 3 - s / 2).into(), (s, s).into()),
                    color,
                    output_scale,
                );
                push_rect(
                    elements,
                    Rectangle::new((x, y + 2 * height / 3 - s / 2).into(), (s, s).into()),
                    color,
                    output_scale,
                );
            }
            '.' => {
                let s = digit_thickness(height).max(1) * 2;
                push_rect(
                    elements,
                    Rectangle::new((x, y + height - s).into(), (s, s).into()),
                    color,
                    output_scale,
                );
            }
            _ => {}
        }
        x += glyph_width(c, height) + gap;
    }
    x
}

/// A small geometric icon standing in for a font glyph, echoing the shape
/// of the actual layout — drawn in a `size` × `size` box anchored at
/// `(x, y)`.
fn draw_layout_icon<R>(
    elements: &mut Vec<CustomRenderElements<R>>,
    mode: LayoutMode,
    x: i32,
    y: i32,
    size: i32,
    color: Color32F,
    output_scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
{
    let t = (size / 8).max(1);
    match mode {
        LayoutMode::Tile => {
            // Master column on the left, two stacked windows on the right —
            // dwm's master-stack, in miniature.
            let master_w = (size * 3) / 5;
            let stack_w = (size - master_w - t).max(1);
            let stack_h = ((size - t) / 2).max(1);
            push_rect(
                elements,
                Rectangle::new((x, y).into(), (master_w, size).into()),
                color,
                output_scale,
            );
            push_rect(
                elements,
                Rectangle::new((x + master_w + t, y).into(), (stack_w, stack_h).into()),
                color,
                output_scale,
            );
            push_rect(
                elements,
                Rectangle::new(
                    (x + master_w + t, y + stack_h + t).into(),
                    (stack_w, stack_h).into(),
                ),
                color,
                output_scale,
            );
        }
        LayoutMode::Monocle => {
            push_rect(
                elements,
                Rectangle::new((x, y).into(), (size, size).into()),
                color,
                output_scale,
            );
        }
        LayoutMode::Floating => {
            // A hollow square outline — four thin strips, the same trick
            // `spitfire.border` uses so it never has to rely on occlusion
            // to punch a hole through anything.
            push_rect(
                elements,
                Rectangle::new((x, y).into(), (size, t).into()),
                color,
                output_scale,
            );
            push_rect(
                elements,
                Rectangle::new((x, y + size - t).into(), (size, t).into()),
                color,
                output_scale,
            );
            push_rect(
                elements,
                Rectangle::new((x, y).into(), (t, size).into()),
                color,
                output_scale,
            );
            push_rect(
                elements,
                Rectangle::new((x + size - t, y).into(), (t, size).into()),
                color,
                output_scale,
            );
        }
        LayoutMode::Fibonacci => {
            // Top+left edges of three shrinking, nested boxes — a hint of
            // the spiral split without drawing the whole thing.
            let (mut rx, mut ry, mut rw, mut rh) = (x, y, size, size);
            for _ in 0..3 {
                push_rect(
                    elements,
                    Rectangle::new((rx, ry).into(), (rw.max(t), t).into()),
                    color,
                    output_scale,
                );
                push_rect(
                    elements,
                    Rectangle::new((rx, ry).into(), (t, rh.max(t)).into()),
                    color,
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
/// strip, workspace list + layout-mode icon on the left, clock/date on the
/// right. Returns nothing if `config.enabled` is `false` — callers can
/// call this unconditionally.
#[allow(clippy::too_many_arguments)]
pub fn bar_elements<R>(
    config: &BarConfig,
    output_width: i32,
    data: &BarData,
    time_text: &str,
    date_text: &str,
    output_scale: Scale<f64>,
) -> Vec<CustomRenderElements<R>>
where
    R: Renderer + ImportAll + ImportMem,
{
    let mut elements = Vec::new();
    if !config.enabled || config.height <= 0 {
        return elements;
    }

    let height = config.height;
    let bg = hex_to_color32f(config.bg);
    let fg = hex_to_color32f(config.fg);
    let fg_active = hex_to_color32f(config.fg_active);

    let margin = (height / 4).max(1);
    let glyph_h = (height - margin * 2).max(1);
    let gap = (glyph_h / 6).max(1);

    // Left: workspace numbers (highlighted when active) + layout-mode icon.
    let mut x = margin;
    for item in &data.workspaces {
        let color = if item.active { fg_active } else { fg };
        x = draw_text(
            &mut elements,
            &item.number.to_string(),
            x,
            margin,
            glyph_h,
            gap,
            color,
            output_scale,
        );
        x += margin;
    }
    draw_layout_icon(
        &mut elements,
        data.mode,
        x,
        margin,
        glyph_h,
        fg,
        output_scale,
    );

    // Right: clock, then date, right-aligned with a gap between them.
    let text = format!("{time_text} {date_text}");
    let text_w = measure_text(&text, glyph_h, gap);
    let start_x = (output_width - margin - text_w).max(margin);
    draw_text(
        &mut elements,
        &text,
        start_x,
        margin,
        glyph_h,
        gap,
        fg,
        output_scale,
    );

    // Background strip across the whole width, top of the output — pushed
    // last, not first: this element list is painter's-algorithm ordered
    // with index 0 on top (the same convention the cursor relies on to
    // stay above windows in `winit.rs`), so an opaque strip pushed *before*
    // the digits/icon above it would just paint over them. Same lesson as
    // `spitfire.border`'s render-order bug, just the opposite end of the
    // list this time.
    push_rect(
        &mut elements,
        Rectangle::new((0, 0).into(), (output_width.max(0), height).into()),
        bg,
        output_scale,
    );

    elements
}
