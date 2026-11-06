//! Memento mori tab: paints memento mori's scene with half blocks, two pixels to a cell. Memento mori
//! paints its own dusk sky, so it looks the same on any terminal theme; the level's land is
//! solid and dark, and the characters are the only bright things on it. Names, epitaphs and
//! narration hang above the characters they belong to.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use reaper_core::mori::{
    BLOCK, DEAD, DUSK, Effect, Fit, Health, LAND, Label, REAPER, Rgb, Stage, Strip, WHITE, ansi256,
};

use super::app::App;
use super::ui::rows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colors {
    True,
    Ansi256,
    /// `NO_COLOR`: shapes carry everything.
    Off,
}

impl Colors {
    pub fn detect() -> Colors {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Colors::Off;
        }
        match std::env::var("COLORTERM").as_deref() {
            Ok("truecolor" | "24bit") => Colors::True,
            _ => Colors::Ansi256,
        }
    }

    fn color(self, Rgb(r, g, b): Rgb) -> Color {
        match self {
            Colors::True => Color::Rgb(r, g, b),
            Colors::Ansi256 => Color::Indexed(ansi256(Rgb(r, g, b)).unwrap_or_else(|| nearest_256(Rgb(r, g, b)))),
            Colors::Off => Color::Reset,
        }
    }

    fn ink(self, rgb: Rgb) -> Style {
        match self {
            Colors::Off => Style::new(),
            _ => Style::new().fg(self.color(rgb)),
        }
    }
}

/// The closest colour in the xterm 256 palette: the 6×6×6 cube or the grey ramp.
fn nearest_256(Rgb(r, g, b): Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| (0..6).min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(v)).abs()).unwrap_or(0);
    let (ri, gi, bi) = (near(r), near(g), near(b));
    let dist = |(x, y, z): (u8, u8, u8)| {
        [(x, r), (y, g), (z, b)].iter().map(|&(a, b)| (i32::from(a) - i32::from(b)).pow(2)).sum::<i32>()
    };
    let grey = ((u32::from(r) + u32::from(g) + u32::from(b)) / 3).saturating_sub(8).div_ceil(10).min(23) as u8;
    let level = 8 + 10 * grey;
    if dist((level, level, level)) < dist((LEVELS[ri], LEVELS[gi], LEVELS[bi])) {
        232 + grey
    } else {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    }
}

/// The dusk on 256-colour terminals, top to bottom: night blues. The nearest colours to the
/// gradient would turn grey near the horizon, the colour of the dead.
const DUSK_256: [u8; 3] = [17, 17, 18];

/// A searchlight's beam, mixed into the dusk: a cool light, since a warm one mixed into the
/// blue comes out grey. On 256-colour terminals, one blue a step up from the sky.
const LIGHT: Rgb = Rgb(0xC8, 0xD4, 0xF4);
const LIGHT_256: u8 = 61;

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let scene = app.mori.scene(app.mori_now(), area.width, area.height);
    app.mori_next.set(scene.next);
    let colors = app.colors;
    let buf = frame.buffer_mut();
    let mut pen = Pen { buf, area, colors, dim: false };
    let back = match &scene.stage {
        Some(stage) => pen.stage(stage, app.prompt_open.then_some(app.prompt_scroll)),
        None => {
            pen.empty(app.mori.sessions() > 0);
            0
        }
    };
    app.prompt_back.set(back);
    pen.strips(&scene.strips);
}

/// What a pixel is: decides its glyph when there's no colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Px {
    Sky,
    Land,
    Crack,
    Block,
    Exit,
    Body,
    /// A strike's wisp, out of the character for a moment.
    Wisp,
    /// A searchlight's beam, where it crosses the sky.
    Light,
}

/// Pixels for one frame: one per column, two per row.
struct Canvas {
    w: i32,
    h: i32,
    pixels: Vec<(Px, Rgb)>,
}

impl Canvas {
    fn set(&mut self, x: i32, y: i32, px: Px, rgb: Rgb) {
        // Row 0 belongs to the strips.
        if (0..self.w).contains(&x) && (2..self.h).contains(&y) {
            self.pixels[(y * self.w + x) as usize] = (px, rgb);
        }
    }

    fn get(&self, x: i32, y: i32) -> (Px, Rgb) {
        self.pixels[(y * self.w + x) as usize]
    }

    /// Level pixel (`x`, `y`), wherever the fit puts it on the screen.
    fn paint(&mut self, fit: &Fit, x: i32, y: i32, px: Px, rgb: Rgb) {
        let Some((sx, sy)) = fit.pixel(x, y) else { return };
        for dy in 0..fit.zoom {
            for dx in 0..fit.zoom {
                self.set(sx + dx, sy + dy, px, rgb);
            }
        }
    }

    /// Light over level pixel (`x`, `y`), if there's sky there: `strength` of the way from the
    /// sky to the light.
    fn light(&mut self, fit: &Fit, x: i32, y: i32, strength: f32) {
        let Some((sx, sy)) = fit.pixel(x, y) else { return };
        for (px, py) in (0..fit.zoom).flat_map(|dy| (0..fit.zoom).map(move |dx| (sx + dx, sy + dy))) {
            if (0..self.w).contains(&px) && (0..self.h).contains(&py) && self.get(px, py).0 == Px::Sky {
                let sky = self.get(px, py).1;
                self.set(px, py, Px::Light, sky.mix(LIGHT, strength));
            }
        }
    }

    /// The top of the land in screen column `x`.
    fn ground(&self, x: i32) -> i32 {
        (2..self.h).find(|&y| self.get(x, y).0 == Px::Land).unwrap_or(self.h)
    }
}

/// Whether a sprite pixel shows while it fades: 3 is solid, 0 is gone.
fn dithered(x: i32, y: i32, fill: u8) -> bool {
    match fill {
        0 => false,
        1 => (x + 2 * y).rem_euclid(3) == 0,
        2 => (x + 2 * y).rem_euclid(3) != 0,
        _ => true,
    }
}

struct Pen<'a> {
    buf: &'a mut Buffer,
    area: Rect,
    colors: Colors,
    dim: bool,
}

impl Pen<'_> {
    /// The sky at pixel row `y`.
    fn sky(&self, y: i32) -> Rgb {
        let t = (y - 2) as f32 / (2 * i32::from(self.area.height) - 3).max(1) as f32;
        let rgb = DUSK.0.mix(DUSK.1, t);
        if self.dim { rgb.dim(0.66) } else { rgb }
    }

    /// The sky's colour on screen at pixel row `y`.
    fn sky_color(&self, y: i32) -> Color {
        match self.colors {
            Colors::Ansi256 => {
                let band = (y.max(0) as usize * DUSK_256.len()) / (2 * usize::from(self.area.height)).max(1);
                Color::Indexed(DUSK_256[band.min(DUSK_256.len() - 1)])
            }
            colors => colors.color(self.sky(y)),
        }
    }

    /// Below the strips, text sits on the sky.
    fn put(&mut self, x: i32, y: i32, text: &str, mut style: Style) {
        if !(0..i32::from(self.area.height)).contains(&y) {
            return;
        }
        if y > 0 && self.colors != Colors::Off && style.bg.is_none() {
            style = style.bg(self.sky_color(2 * y));
        }
        for (i, ch) in text.chars().enumerate() {
            let cx = x + i as i32;
            if !(0..i32::from(self.area.width)).contains(&cx) {
                continue;
            }
            if let Some(cell) = self.buf.cell_mut((self.area.x + cx as u16, self.area.y + y as u16)) {
                cell.reset();
                cell.set_char(ch).set_style(style);
            }
        }
    }

    fn ink(&self, rgb: Rgb, fade: f32, y: i32) -> Style {
        match self.colors {
            Colors::Off => Style::new(),
            colors => Style::new().fg(colors.color(rgb.mix(self.sky(2 * y), fade))),
        }
    }

    fn empty(&mut self, cleared: bool) {
        let (w, mid) = (i32::from(self.area.width), i32::from(self.area.height) / 2);
        let lines = if cleared {
            [
                ("Nobody here now.", Style::new().add_modifier(Modifier::BOLD)),
                (
                    "A session is cleared away a minute after the last of it dies. Press d for a death anyway.",
                    Style::new().fg(Color::DarkGray),
                ),
            ]
        } else {
            [
                ("Nobody here yet.", Style::new().add_modifier(Modifier::BOLD)),
                (
                    "Agents appear here, and get a name, as soon as a guarded session starts.",
                    Style::new().fg(Color::DarkGray),
                ),
            ]
        };
        for (i, (text, style)) in lines.into_iter().enumerate() {
            self.put((w - text.chars().count() as i32) / 2, mid - 1 + i as i32, text, style);
        }
    }

    /// With the prompts open, `Some` of how many rows back from the latest they're scrolled.
    /// Returns how far back they go.
    fn stage(&mut self, stage: &Stage, prompts: Option<usize>) -> usize {
        self.dim = stage.dim;
        let fit = &stage.fit;
        let (w, h) = (i32::from(self.area.width), 2 * i32::from(self.area.height));
        let mut canvas = Canvas { w, h, pixels: (0..w * h).map(|i| (Px::Sky, self.sky(i / w.max(1)))).collect() };
        let land = if stage.dim { LAND.dim(0.66) } else { LAND };
        for (x, &top) in stage.land.iter().enumerate() {
            for y in top..2 * fit.band {
                canvas.paint(fit, x as i32, y, Px::Land, land);
            }
        }
        for &(x, y) in &stage.cracks {
            canvas.paint(fit, x, y, Px::Crack, LAND);
        }
        // What deaths leave behind soaks into the ground: without colour, it's just ground.
        for &(x, y, rgb) in &stage.stains {
            canvas.paint(fit, x, y, Px::Land, rgb);
        }
        for block in &stage.blocks {
            for y in block.y..block.y + block.h {
                for x in block.x..block.x + block.w {
                    canvas.paint(fit, x, y, Px::Block, BLOCK);
                }
            }
        }
        // Exits: a doorway in each character's own shape, faint while it waits, lit once its
        // character is home.
        for exit in &stage.exits {
            let top = exit.bottom - exit.h;
            for y in top..=exit.bottom {
                let sky = self.sky(fit.pixel(exit.x, y).map_or(y, |p| p.1));
                let rgb = if exit.lit { exit.hue.mix(WHITE, 0.6) } else { exit.hue.mix(sky, 0.55) };
                for x in exit.x - 1..=exit.x + exit.w {
                    if y == top || x == exit.x - 1 || x == exit.x + exit.w {
                        canvas.paint(fit, x, y, Px::Exit, rgb);
                    }
                }
            }
        }
        // Only without colour are the dead outlines, to tell them from the sleeping.
        for actor in stage.actors.iter().filter(|a| !a.hidden) {
            let top = actor.bottom - actor.h + 1;
            if actor.grave {
                // A little cross: the upright down the middle, the arm across one below the top.
                let mid = actor.x + actor.w / 2;
                for y in top..=actor.bottom {
                    canvas.paint(fit, mid, y, Px::Body, actor.fill);
                }
                for x in actor.x..actor.x + actor.w {
                    canvas.paint(fit, x, top + 1, Px::Body, actor.fill);
                }
                continue;
            }
            let hollow = actor.outline && self.colors == Colors::Off;
            for y in top..=actor.bottom {
                for x in actor.x..actor.x + actor.w {
                    let edge = y == top || y == actor.bottom || x == actor.x || x == actor.x + actor.w - 1;
                    if !hollow || edge {
                        canvas.paint(fit, x, y, Px::Body, actor.fill);
                    }
                }
            }
        }
        for effect in &stage.effects {
            match effect {
                Effect::Sprite { sprite, x, bottom, flip, fill, clip } => {
                    for (sx, sy, rgb) in sprite.pixels() {
                        let px = if *flip { x + sprite.w() - 1 - sx } else { x + sx };
                        let py = bottom - sprite.h() + 1 + sy;
                        if py <= *clip && dithered(px, py, *fill) {
                            canvas.paint(fit, px, py, Px::Body, rgb);
                        }
                    }
                }
                Effect::Pixels(pixels) => {
                    pixels.iter().for_each(|&(x, y, rgb)| canvas.paint(fit, x, y, Px::Body, rgb));
                }
                Effect::Light(pixels) => pixels.iter().for_each(|&(x, y, strength)| canvas.light(fit, x, y, strength)),
                Effect::Wisp { pixels, hue } => {
                    pixels.iter().for_each(|&(x, y)| canvas.paint(fit, x, y, Px::Wisp, *hue));
                }
            }
        }
        self.blit(&canvas);
        self.health(stage.health.as_ref(), &canvas);

        // Labels never sit on each other: the picked character's goes first, then the living,
        // then epitaphs; one that would land on another moves up a row, or isn't shown.
        let mut order: Vec<&reaper_core::mori::Actor> = stage.actors.iter().collect();
        order.sort_by_key(|a| (!a.picked, matches!(a.label, Some(Label::Epitaph(_)))));
        let mut taken: Vec<(i32, i32, i32)> = Vec::new();
        for actor in order {
            // Names ride along when it jumps and rise above a death scene; epitaphs sit on the outline.
            let above = (actor.bottom - actor.h + 1).div_euclid(2) - 1;
            let row = match actor.label {
                Some(Label::Epitaph(_)) => above,
                _ => above.min(actor.voice.1),
            };
            let Some((centre, row)) = fit.cell(actor.x + actor.w / 2, row) else { continue };
            let (text, style) = match actor.label {
                Some(Label::Name { faint }) => {
                    let mut style = self.ink(actor.hue, if faint { 0.5 } else { 0.0 }, row);
                    if actor.picked {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if faint && self.colors == Colors::Off {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    (actor.name.to_string(), style)
                }
                Some(Label::Says(words)) => {
                    (words.to_string(), self.ink(actor.hue, 0.0, row).add_modifier(Modifier::BOLD))
                }
                Some(Label::Epitaph(word)) => {
                    let style = match self.colors {
                        Colors::Off => Style::new().add_modifier(Modifier::DIM),
                        _ => self.ink(DEAD.mix(WHITE, 0.25), 0.0, row),
                    };
                    (word.to_string(), style)
                }
                None => continue,
            };
            let len = text.chars().count() as i32;
            let x = (centre - len / 2).clamp(0, (i32::from(self.area.width) - len).max(0));
            let free = |row: i32| taken.iter().all(|&(r, a, b)| r != row || x + len < a || x > b);
            let Some(row) = [row, row - 1].into_iter().find(|&r| free(r)) else { continue };
            taken.push((row, x - 1, x + len));
            self.put(x, row, &text, style);
            if actor.picked {
                let style = self.ink(actor.hue, 0.0, row);
                self.put(x - 2, row, "▾", style);
            }
        }

        let mut said = None;
        if let Some((n, (nx, nrow))) = stage.narration.as_ref().and_then(|n| Some((n, fit.cell(n.x, n.row)?))) {
            let cols = i32::from(self.area.width);
            let lines = wrap(&n.text, (cols - 4).clamp(12, 72) as usize);
            let top = (nrow - lines.len() as i32).max(1);
            said = Some(top);
            let mut left = n.shown;
            for (i, line) in lines.iter().enumerate() {
                let row = top + i as i32;
                let style = match (self.colors, n.faint) {
                    (Colors::Off, 0) => Style::new(),
                    (Colors::Off, 1) => Style::new().add_modifier(Modifier::DIM),
                    (Colors::Off, _) => Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                    (_, faint) => self.ink(WHITE, [0.0, 0.45, 0.75][usize::from(faint.min(2))], row),
                };
                let len = line.chars().count() as i32;
                let x = (nx - len / 2).clamp(1, (cols - 1 - len).max(1));
                let typed: String = line.chars().take(left).collect();
                left = left.saturating_sub(line.chars().count() + 1);
                // A cell of sky either side keeps the land and bodies from touching the words.
                self.put(x - 1, row, &format!(" {typed} "), style);
            }
        }

        if let Some(lit) = stage.filming {
            let x = i32::from(self.area.width) - 7;
            let dot = match self.colors {
                Colors::Off => Style::new(),
                _ => self.ink(REAPER, 0.0, 1),
            };
            self.put(x, 1, if lit { "●" } else { " " }, dot);
            self.put(x + 2, 1, "REC", self.ink(WHITE, 0.0, 1).add_modifier(Modifier::BOLD));
        }

        if let Some(title) = &stage.title {
            let mut style = self.ink(WHITE, if title.faint { 0.5 } else { 0.0 }, 1).add_modifier(Modifier::BOLD);
            if title.faint && self.colors == Colors::Off {
                style = style.add_modifier(Modifier::DIM);
            }
            let x = (i32::from(self.area.width) - title.text.chars().count() as i32) / 2;
            self.put(x, 1, &title.text, style);
        }

        // Opened, they cover the level; closed, the latest gives way to the title card and the
        // narrator.
        match (stage.prompts.last(), prompts) {
            (Some(_), Some(back)) => return self.prompts(&stage.prompts, back),
            (Some(latest), None) if stage.title.is_none() && said != Some(1) => {
                self.prompt_line(latest, stage.filming.is_some());
            }
            _ => {}
        }
        0
    }

    /// Row 1, faint: the start of what the human asked the session for last.
    fn prompt_line(&mut self, prompt: &str, filming: bool) {
        let room = usize::from(self.area.width).saturating_sub(if filming { 10 } else { 2 });
        let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = if flat.chars().count() <= room {
            flat
        } else {
            let mut cut: String = flat.chars().take(room.saturating_sub(1)).collect();
            cut.push('…');
            cut
        };
        self.put(1, 1, &text, self.faint(1));
    }

    /// From row 1 down, over the level: everything the human asked the session for, each in its
    /// own lines, the latest at the bottom in white and the ones before faint over a rule. As
    /// much as fits, `back` rows up from the end, with a scrollbar in the right margin when it
    /// doesn't all fit. Returns how far back it goes.
    fn prompts(&mut self, prompts: &[String], back: usize) -> usize {
        let width = usize::from(self.area.width).saturating_sub(2).max(1);
        let most = usize::from(self.area.height).saturating_sub(3).max(1);
        // Each row, and whether it's from the latest.
        let mut lines: Vec<(String, bool)> = Vec::new();
        for (i, prompt) in prompts.iter().enumerate() {
            let latest = i + 1 == prompts.len();
            if i > 0 {
                lines.push(("─".repeat(width), false));
            }
            let start = lines.len();
            for source in prompt.lines() {
                match rows(source, width) {
                    // One blank line between paragraphs, however many there were.
                    rows if rows.is_empty() => {
                        if lines.len() > start && lines.last().is_some_and(|(l, _)| !l.is_empty()) {
                            lines.push((String::new(), latest));
                        }
                    }
                    rows => lines.extend(rows.into_iter().map(|row| (row, latest))),
                }
            }
            if lines.len() > start && lines.last().is_some_and(|(l, _)| l.is_empty()) {
                lines.pop();
            }
        }
        let furthest = lines.len().saturating_sub(most);
        let back = back.min(furthest);
        let end = lines.len() - back;
        let shown = &lines[end.saturating_sub(most)..end];
        let thumb = (furthest > 0).then(|| {
            let len = (most * most / lines.len()).max(1);
            let top = (furthest - back) * (most - len) / furthest;
            top..top + len
        });
        for (i, (line, latest)) in shown.iter().enumerate() {
            let row = 1 + i as i32;
            let style = if *latest { self.ink(WHITE, 0.0, row) } else { self.faint(row) };
            self.put(0, row, &format!(" {line:<width$} "), style);
            if thumb.as_ref().is_some_and(|t| t.contains(&i)) {
                self.put(1 + width as i32, row, "┃", self.faint(row));
            }
        }
        // A blank row under it keeps the words off the level.
        self.put(0, 1 + shown.len() as i32, &" ".repeat(width + 2), Style::new());
        furthest
    }

    /// Words that step back: half into the sky, or dim without colour.
    fn faint(&self, y: i32) -> Style {
        let style = self.ink(WHITE, 0.5, y);
        if self.colors == Colors::Off { style.add_modifier(Modifier::DIM) } else { style }
    }

    /// The top left corner: the session's health bar, if it fits above the land there, and the
    /// session's strikes after it. What kills took is reaper's red, the count too; a dead main
    /// agent's bar flatlines in grey, its count gone with it (its grave says it died).
    fn health(&mut self, health: Option<&Health>, canvas: &Canvas) {
        const BAR: usize = 8;
        const ROW: i32 = 2;
        const PARTS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];
        let Some(h) = health else { return };
        let strikes = if h.dead || h.strikes == 0 { String::new() } else { format!(" {}", h.strikes) };
        let names = h.name.chars().count();
        let width = 1 + names + 1 + BAR + strikes.chars().count();
        // Its lower pixel stays at least a pixel above the land under it.
        let ground = (0..=width as i32).filter(|&x| x < canvas.w).map(|x| canvas.ground(x)).min().unwrap_or(i32::MAX);
        if (ground - 3).div_euclid(2) < ROW {
            return;
        }
        let row = ROW;
        let hue = match (h.dead, h.asleep) {
            (true, _) => DEAD,
            (_, true) => h.hue.dim(0.6),
            _ => h.hue,
        };
        let mut style = self.ink(hue, 0.0, row);
        if self.colors == Colors::Off && (h.dead || h.asleep) {
            style = style.add_modifier(Modifier::DIM);
        }
        self.put(1, row, &format!("{:<names$} ", h.name), style);
        let x = 2 + names as i32;
        if h.dead {
            self.put(x, row, &"─".repeat(BAR), style);
            return;
        }
        let eighths = (h.left * (BAR * 8) as f32).round() as usize;
        let (full, part) = (eighths / 8, eighths % 8);
        let lost = REAPER.mix(self.sky(2 * row), 0.35);
        let (trough, edge) = match self.colors {
            Colors::Off => ("░", Style::new()),
            colors => ("█", style.bg(colors.color(lost))),
        };
        let empty = self.ink(lost, 0.0, row);
        let mut bar = vec![("█".repeat(full), style)];
        if full < BAR {
            bar.push(if part == 0 { (trough.into(), empty) } else { (PARTS[part - 1].into(), edge) });
            bar.push((trough.repeat(BAR - full - 1), empty));
        }
        let mut cx = x;
        for (text, style) in bar {
            self.put(cx, row, &text, style);
            cx += text.chars().count() as i32;
        }
        self.put(cx, row, &strikes, self.ink(REAPER, 0.0, row));
    }

    fn blit(&mut self, canvas: &Canvas) {
        for row in 1..canvas.h / 2 {
            for col in 0..canvas.w {
                let (top, bottom) = (canvas.get(col, 2 * row), canvas.get(col, 2 * row + 1));
                let color = |(px, rgb): (Px, Rgb), y: i32| match px {
                    Px::Sky | Px::Crack => self.sky_color(y),
                    Px::Light if self.colors == Colors::Ansi256 => Color::Indexed(LIGHT_256),
                    _ => self.colors.color(rgb),
                };
                let (t, b) = (color(top, 2 * row), color(bottom, 2 * row + 1));
                let Some(cell) = self.buf.cell_mut((self.area.x + col as u16, self.area.y + row as u16)) else {
                    continue;
                };
                cell.reset();
                if self.colors == Colors::Off {
                    cell.set_symbol(glyph(top.0, bottom.0));
                    continue;
                }
                match (top.0, bottom.0) {
                    (Px::Sky, Px::Sky) => cell.set_symbol(" ").set_bg(t),
                    _ if t == b => cell.set_symbol("█").set_fg(t),
                    _ => cell.set_symbol("▀").set_fg(t).set_bg(b),
                };
            }
        }
    }

    /// Row 0: every session at a glance.
    fn strips(&mut self, strips: &[Strip]) {
        if strips.is_empty() {
            return;
        }
        let parts: Vec<Vec<(String, Style)>> = strips.iter().map(|s| self.strip(s)).collect();
        let width = |p: &Vec<(String, Style)>| p.iter().map(|(t, _)| t.chars().count()).sum::<usize>() + 2;
        let mut order: Vec<usize> = (0..strips.len()).collect();
        order.sort_by_key(|&i| (!strips[i].followed, std::cmp::Reverse(strips[i].last_at)));
        let room = usize::from(self.area.width).saturating_sub(10);
        let (mut used, mut shown) = (0, Vec::new());
        for i in order {
            if used + width(&parts[i]) <= room || shown.is_empty() {
                used += width(&parts[i]);
                shown.push(i);
            }
        }
        shown.sort_unstable();
        let mut x = 1;
        for &i in &shown {
            for (text, style) in &parts[i] {
                self.put(x, 0, text, *style);
                x += text.chars().count() as i32;
            }
            x += 2;
        }
        let more = strips.len() - shown.len();
        if more > 0 {
            self.put(x, 0, &format!("+{more} more"), Style::new().fg(Color::DarkGray));
        }
    }

    fn strip(&self, strip: &Strip) -> Vec<(String, Style)> {
        let number = format!(" {} ", strip.number);
        if strip.followed {
            return vec![(number, Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED))];
        }
        let style = if strip.flash && self.colors != Colors::Off {
            Style::new().fg(Color::Black).bg(self.colors.color(REAPER))
        } else if strip.flash {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        let mut parts = vec![(number, style)];
        for mini in &strip.minis {
            if mini.alive && mini.asking {
                // Signalling: it needs the human.
                parts.push(("!".into(), self.colors.ink(mini.hue).add_modifier(Modifier::BOLD)));
                continue;
            }
            let (glyph, rgb) = if !mini.alive {
                ("†", DEAD)
            } else if mini.asleep {
                // Lying down, in its own colour: asleep, not gone.
                ("▂", mini.hue.dim(0.6))
            } else if mini.waiting {
                // Hollow while it waits for something to do: only those at work are solid.
                ("▫", mini.hue.dim(0.6))
            } else {
                ("▪", mini.hue)
            };
            parts.push((glyph.into(), self.colors.ink(rgb)));
        }
        if let Some(line) = &strip.line {
            parts.push((format!(" {line}"), Style::new().add_modifier(Modifier::DIM | Modifier::ITALIC)));
        }
        parts
    }
}

/// Without colour, a cell's glyph says what's in it.
fn glyph(top: Px, bottom: Px) -> &'static str {
    match (top, bottom) {
        (Px::Body, Px::Body) => "█",
        (Px::Body, _) => "▀",
        (_, Px::Body) => "▄",
        (Px::Exit | Px::Wisp | Px::Light, _) | (_, Px::Exit | Px::Wisp | Px::Light) => "░",
        (Px::Crack, _) | (_, Px::Crack) => "▼",
        (Px::Block, _) | (_, Px::Block) => "▒",
        (Px::Land, Px::Land) => "▓",
        (Px::Land, _) => "▀",
        (_, Px::Land) => "▄",
        _ => " ",
    }
}

fn wrap(text: &str, max: usize) -> Vec<String> {
    let len = text.chars().count();
    if len <= max {
        return vec![text.to_string()];
    }
    let spaces = text.char_indices().filter(|&(_, c)| c == ' ').map(|(i, _)| i);
    let balanced = spaces
        .filter(|&i| text[..i].chars().count() <= max && text[i + 1..].chars().count() <= max)
        .min_by_key(|&i| text[..i].chars().count().abs_diff(len / 2));
    if let Some(i) = balanced {
        return vec![text[..i].to_string(), text[i + 1..].to_string()];
    }
    let mut lines = vec![String::new()];
    for word in text.split(' ') {
        let line = lines.last_mut().expect("starts with one");
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > max {
            lines.push(word.to_string());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use reaper_core::mori::HALO;

    #[test]
    fn the_256_palette_matches_the_skill_table() {
        let paint = |rgb| Colors::Ansi256.color(rgb);
        for (rgb, index) in [
            (Rgb(0xE8, 0xA3, 0x3D), 179),
            (Rgb(0x3F, 0xB8, 0xAF), 73),
            (Rgb(0x9B, 0x7F, 0xD4), 140),
            (Rgb(0x5A, 0xA9, 0xE6), 74),
            (Rgb(0x7B, 0xC6, 0x7E), 114),
            (Rgb(0xD9, 0xC2, 0x7A), 186),
            (Rgb(0x8F, 0xE3, 0xC8), 121),
            (Rgb(0x5D, 0x6B, 0xD8), 62),
            (REAPER, 167),
            (DEAD, 59),
            (HALO, 229),
        ] {
            assert_eq!(paint(rgb), Color::Indexed(index), "{rgb:?}");
        }
        // Colours outside the palette (dimmed, mixed, sprites) take the nearest.
        assert_eq!(nearest_256(Rgb(0, 0, 0)), 16);
        assert_eq!(nearest_256(Rgb(0x80, 0x80, 0x80)), 244);
        assert_eq!(nearest_256(Rgb(0xff, 0xff, 0xff)), 231);
        // The sky never takes the colour of the dead, and nor does light across it.
        assert!(!DUSK_256.contains(&59));
        assert_ne!(LIGHT_256, 59);
    }

    #[test]
    fn light_shows_only_where_there_is_sky() {
        let fit = Fit::new(80, 24);
        let sky = Rgb(0x14, 0x18, 0x30);
        let mut canvas = Canvas { w: 80, h: 48, pixels: vec![(Px::Sky, sky); 80 * 48] };
        canvas.paint(&fit, 10, 20, Px::Land, LAND);
        canvas.light(&fit, 10, 20, 0.5);
        canvas.light(&fit, 11, 20, 0.5);
        let at = |x| fit.pixel(x, 20).map(|(sx, sy)| canvas.get(sx, sy)).unwrap();
        assert_eq!(at(10), (Px::Land, LAND), "the land stays dark");
        assert_eq!(at(11), (Px::Light, sky.mix(LIGHT, 0.5)));
    }

    #[test]
    fn narration_wraps_into_balanced_lines() {
        assert_eq!(wrap("short", 20), ["short"]);
        assert_eq!(wrap("one two three four", 12), ["one two", "three four"]);
        assert_eq!(wrap("aaaa bbbb cccc dddd eeee", 9), ["aaaa bbbb", "cccc dddd", "eeee"]);
    }
}
