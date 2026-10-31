//! What memento mori looks like at one moment. [`Mori::scene`] is pure: the same memento mori, time
//! and size always give the same scene, which is what makes memento mori testable.
//!
//! Coordinates are pixels from the top left of memento mori: a terminal cell is one pixel wide
//! and two tall, so pixel row `y` is in cell row `y / 2`. Text positions are cell rows.

use std::collections::{BTreeMap, HashMap};

use super::cast::{self, DEAD, Personality, Rgb, Shape, WHITE};
use super::level::{Leg, Level, Spot};
use super::lines::{Section, When};
use super::sprites::{self, BASKET, BLADE, BUNNY, FRAME as GUILLOTINE, GRENADE, GRIM, PIN, SAUCER, Sprite, WOLF};
use super::{Act, Beat, Character, DEPART, GIVE, LEAVE, Method, Moment, Mori, PRESS, PUSH_FLY, PUSH_LEAN, SPLIT};
use crate::state::Status;

/// How often to redraw while something moves.
pub const FRAME: i64 = 50;

/// How long names stay up after a character arrives, is stopped, or the stage changes.
const NAME_FOR: i64 = 4000;
const TITLE_FOR: i64 = 2300;
const STRIP_FLASH_FOR: i64 = 800;
/// A session where everyone has been waiting this long gets a line about it.
const QUIET_AFTER: i64 = 60_000;
const FADE: i64 = 600;
/// How much of its colour a sleeping character keeps: its own hue, never the grey of the dead.
const ASLEEP: f32 = 0.45;
const CROSS: (i32, i32) = (3, 5);

/// Blood: red, which is otherwise reaper's alone, lent to a death scene while it plays.
const BLOOD: Rgb = Rgb(0xC4, 0x1E, 0x2C);
/// Blood once the scene is over, dried dark into the ground.
const DRIED: Rgb = Rgb(0x4E, 0x16, 0x1E);
const WATER: Rgb = Rgb(0x9C, 0xC8, 0xEE);
/// The gold at the edge of the holy hand grenade's blast, and the halo of the enlightened.
const HALO: Rgb = Rgb(0xF5, 0xE6, 0xA0);
const BEAM: Rgb = Rgb(0xB8, 0xF0, 0xC8);

pub(super) fn typing(text: &str) -> i64 {
    (text.chars().count() as i64 * 15).clamp(600, 1800)
}

/// Long enough to read.
fn hold(text: &str) -> i64 {
    1500 + text.chars().count() as i64 * 40
}

pub(super) fn life(text: &str) -> i64 {
    typing(text) + hold(text) + FADE
}

#[derive(Debug, Clone)]
pub struct Scene {
    /// Every session in the order of their numbers; the followed one is on the stage.
    pub strips: Vec<Strip>,
    pub stage: Option<Stage>,
    /// When the scene next changes by itself, if it does before the next event.
    pub next: Option<i64>,
}

/// A session at a glance, on the row above the stage.
#[derive(Debug, Clone)]
pub struct Strip {
    pub number: usize,
    pub followed: bool,
    pub minis: Vec<Mini>,
    /// Someone there was just stopped.
    pub flash: bool,
    /// One short line after a death there.
    pub line: Option<String>,
    pub last_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mini {
    pub hue: Rgb,
    pub alive: bool,
    /// Reaped while idle, until resumed.
    pub asleep: bool,
    /// In its exit, its turn over: not at work.
    pub home: bool,
}

/// The followed session's level, and everyone on it.
#[derive(Debug, Clone)]
pub struct Stage {
    pub number: usize,
    /// What the human first asked the session for, as logged (cut short).
    pub prompt: Option<String>,
    pub title: Option<Title>,
    /// The lights are down: the reaper has come.
    pub dim: bool,
    /// It's being filmed, the lemmings' nature documentary: whether the light is on just now.
    pub filming: Option<bool>,
    /// The top pixel row of the land in each column. It's solid from there to the bottom.
    pub land: Vec<i32>,
    /// Pixels cracked out of the land where characters hit the invisible edge.
    pub cracks: Vec<(i32, i32)>,
    /// What deaths leave on the ground: blood, and the odd piece. Drawn over the land.
    pub stains: Vec<(i32, i32, Rgb)>,
    /// The loose blocks on the bench, and any one being pushed.
    pub blocks: Vec<Rect>,
    /// Each character's exit, an outline of its own shape, waiting for it.
    pub exits: Vec<Exit>,
    pub actors: Vec<Actor>,
    /// Everyone on the stage, in the top left corner: what each has left before it's stopped.
    pub health: Vec<Health>,
    /// Drawn over the actors, in order.
    pub effects: Vec<Effect>,
    pub narration: Option<Narration>,
}

/// Left column, top pixel row, and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A doorway in the shape of its character: the body it frames is `w` × `h` with its bottom
/// left at (`x`, `bottom`), and the frame runs one pixel outside that, open at the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    pub x: i32,
    pub bottom: i32,
    pub w: i32,
    pub h: i32,
    pub hue: Rgb,
    /// Its character is home in it.
    pub lit: bool,
}

/// A character's health bar: a kill takes a piece of it, nothing gives it back, and the
/// character is stopped once it's empty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Health {
    pub name: &'static str,
    pub hue: Rgb,
    /// From 1, untouched, to 0.
    pub left: f32,
    pub dead: bool,
    pub asleep: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub text: String,
    pub faint: bool,
}

#[derive(Debug, Clone)]
pub struct Actor {
    pub agent: String,
    pub name: &'static str,
    pub hue: Rgb,
    /// The body: left column, bottom pixel row, size.
    pub x: i32,
    pub bottom: i32,
    pub w: i32,
    pub h: i32,
    pub fill: Rgb,
    /// The dead: solid grey, and an outline where there's no colour to say so.
    pub outline: bool,
    pub hidden: bool,
    pub alive: bool,
    /// Dead, its scene over: a little grave cross, the size of `w` × `h`, stands where it lies.
    pub grave: bool,
    /// In its exit, its turn over: it fills the outline of its own shape.
    pub home: bool,
    pub label: Option<Label>,
    pub picked: bool,
    /// Where lines about it go: its centre column and the cell row just above its head at rest
    /// (above its outline, once dead). Names hang here too while a death scene plays.
    pub voice: (i32, i32),
    /// Where the line about its death goes: above the whole scene, so it stays put and readable.
    last_words: (i32, i32),
    index: usize,
}

/// The word or two over a character: its name, what it says out loud in a death scene (the holy
/// hand grenade's count), or its epitaph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Name { faint: bool },
    Says(&'static str),
    Epitaph(&'static str),
}

#[derive(Debug, Clone)]
pub enum Effect {
    /// `fill` fades it in and out: 3 is solid, 0 is gone. Pixels below row `clip` are hidden.
    Sprite {
        sprite: &'static Sprite,
        x: i32,
        bottom: i32,
        flip: bool,
        fill: u8,
        clip: i32,
    },
    Pixels(Vec<(i32, i32, Rgb)>),
    /// The invisible edge, visible for a moment where an agent touched it.
    Flash {
        x: i32,
        top: i32,
        bottom: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Narration {
    pub text: String,
    /// Characters typed so far.
    pub shown: usize,
    /// 0 while it holds, then 1 and 2 as it fades.
    pub faint: u8,
    /// The centre column and cell row of the head it hangs above.
    pub x: i32,
    pub row: i32,
}

#[derive(Debug, Clone, Copy)]
struct Pace {
    now: i64,
    moving: bool,
    next: Option<i64>,
}

impl Pace {
    fn moving(&mut self) {
        self.moving = true;
    }

    fn at(&mut self, t: i64) {
        if t > self.now {
            self.next = Some(self.next.map_or(t, |n| n.min(t)));
        }
    }

    fn next(&self) -> Option<i64> {
        if self.moving { Some(self.now + FRAME) } else { self.next }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Body {
    x: i32,
    bottom: i32,
    w: i32,
    h: i32,
    fill: Rgb,
    outline: bool,
    hidden: bool,
}

impl Body {
    fn top(&self) -> i32 {
        self.bottom - self.h + 1
    }

    fn laid(self, shape: Shape) -> Body {
        let flat = shape.flat();
        Body { x: self.x + self.w / 2 - flat.w / 2, w: flat.w, h: flat.h, fill: DEAD, outline: true, ..self }
    }
}

struct Pose {
    body: Body,
    epitaph: Option<&'static str>,
    /// In its exit.
    home: bool,
    /// Where a dying character stood when its scene began.
    stood: Option<Body>,
    /// The top pixel row of its death scene.
    scene_top: i32,
    /// What it says out loud just now, in place of its name.
    says: Option<&'static str>,
}

fn lerp(a: i32, b: i32, p: f32) -> i32 {
    (a as f32 + (b - a) as f32 * p.clamp(0.0, 1.0)).round() as i32
}

/// Height above the straight line at `p` of a jump `height` pixels high.
fn arc(p: f32, height: f32) -> i32 {
    (4.0 * height * p * (1.0 - p)).round() as i32
}

fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p) * (1.0 - p)
}

fn ease_in_out(p: f32) -> f32 {
    if p < 0.5 { 2.0 * p * p } else { 1.0 - 2.0 * (1.0 - p) * (1.0 - p) }
}

fn progress(e: i64, start: i64, len: i64) -> f32 {
    if len <= 0 { 1.0 } else { ((e - start) as f32 / len as f32).clamp(0.0, 1.0) }
}

/// Milliseconds to fall `drop` pixels, picking up speed.
fn falling(drop: i32) -> i64 {
    (300.0 * (drop.max(1) as f32 / 10.0).sqrt()).round() as i64
}

fn dust() -> Rgb {
    sprites::ink(b'D').expect("dust is in the palette")
}

fn puff(from: i32, to: i32, bottom: i32, e: i64) -> Effect {
    let pixels = (from..to)
        .flat_map(|x| [(x, bottom - 1), (x, bottom - 2)])
        .filter(|&(x, y)| cast::hash(&[&x.to_le_bytes(), &y.to_le_bytes(), &(e / 60).to_le_bytes()]).is_multiple_of(3))
        .map(|(x, y)| (x, y, dust()))
        .collect();
    Effect::Pixels(pixels)
}

impl Mori {
    /// Memento mori at time `now`, for an area `cols` × `rows` cells.
    pub fn scene(&self, now: i64, cols: u16, rows: u16) -> Scene {
        let mut pace = Pace { now, moving: false, next: None };
        let followed = self.shown(now);
        // Sessions where everyone has died leave a while later; redraw when they do.
        (0..self.sessions.len()).filter_map(|s| self.gone_at(s)).for_each(|at| pace.at(at));
        let strips = self.on_screen(now).into_iter().map(|s| self.strip(s, followed == Some(s), &mut pace)).collect();
        let stage = followed.map(|s| {
            let level = Level::new(&self.sessions[s].plan, cols, rows);
            let mut build = Build::new(self, s, level, pace);
            let stage = build.stage();
            pace = build.pace;
            stage
        });
        Scene { strips, stage, next: pace.next() }
    }

    fn strip(&self, s: usize, followed: bool, pace: &mut Pace) -> Strip {
        let session = &self.sessions[s];
        let now = pace.now;
        for at in session.members.iter().filter_map(|&m| self.actors[m].ended) {
            pace.at(at + DEPART);
        }
        let minis = session
            .members
            .iter()
            .filter(|&&m| self.here(m, now))
            .map(|&m| {
                let c = &self.actors[m];
                let dead = c.death.is_some_and(|d| d.at <= now) && !c.alive;
                Mini { hue: c.hue, alive: !dead, asleep: c.asleep_at(now), home: c.home_at(now) }
            })
            .collect();
        let flash = session.kill_at.is_some_and(|k| now >= k && now < k + STRIP_FLASH_FOR);
        if let Some(k) = session.kill_at {
            pace.at(k);
            pace.at(k + STRIP_FLASH_FOR);
        }
        // A death's line beside the strip stays until the session does anything newer.
        let line = session.death.as_ref().and_then(|(at, line)| {
            pace.at(*at);
            (now >= *at && session.last_at <= *at).then(|| line.clone())
        });
        Strip { number: session.number, followed, minis, flash, line, last_at: session.last_at }
    }

    /// Who is on the stage: everyone, unless there's no room, when the dead go first (oldest
    /// first), then the sleeping, then whoever has waited longest.
    fn on_stage(&self, s: usize, cols: i32, now: i64) -> Vec<usize> {
        let mut on: Vec<usize> = self.sessions[s].members.iter().copied().filter(|&m| self.here(m, now)).collect();
        let room = (cols / 5).clamp(2, 24) as usize;
        if on.len() > room {
            let mut dead: Vec<usize> = on.iter().copied().filter(|&m| !self.actors[m].alive).collect();
            dead.sort_by_key(|&m| self.actors[m].death.map_or(i64::MIN, |d| d.at));
            let mut asleep: Vec<usize> = on.iter().copied().filter(|&m| self.actors[m].asleep).collect();
            asleep.sort_by_key(|&m| self.actors[m].last_at);
            let mut idle: Vec<usize> = on
                .iter()
                .copied()
                .filter(|&m| self.actors[m].alive && matches!(self.status(m), Some(Status::Idle | Status::Ended)))
                .collect();
            idle.sort_by_key(|&m| self.actors[m].last_at);
            for m in dead.into_iter().chain(asleep).chain(idle) {
                if on.len() <= room {
                    break;
                }
                on.retain(|&x| x != m);
            }
        }
        on
    }

    /// The line playing now in session `s`: among the lines still showing, a newer one takes
    /// over if it matters at least as much, or answers a key press. Nothing queues. A death's
    /// line doesn't fade: it stays up until the session says anything newer, which takes over
    /// whatever it is, and then it's done.
    fn moment(&self, s: usize, pace: &mut Pace) -> Option<Moment> {
        let session = &self.sessions[s];
        let now = pace.now;
        let picked = self.view.picked;
        let mut moments: Vec<&Moment> = session.moments.iter().collect();
        let quiet = self.quiet_line(s, pace);
        moments.extend(quiet.as_ref());
        moments.sort_by_key(|m| m.at);
        let said = |after: i64| moments.iter().any(|n| n.at > after && n.at <= now);
        let mut current: Option<&Moment> = None;
        for &m in &moments {
            if m.at > now {
                pace.at(m.at);
                continue;
            }
            let over = if m.lasts() { said(m.at) } else { now >= m.at + life(&m.text) };
            if over || m.quiet && picked.is_some_and(|p| p != m.actor) {
                continue;
            }
            if current.is_none_or(|c| m.view || m.prio >= c.prio || c.lasts()) {
                current = Some(m);
            }
        }
        current.cloned()
    }

    /// The sleeping aren't waiting for anything, so they say nothing.
    fn quiet_line(&self, s: usize, pace: &mut Pace) -> Option<Moment> {
        let session = &self.sessions[s];
        let alive: Vec<usize> = session
            .members
            .iter()
            .copied()
            .filter(|&m| self.actors[m].alive && !self.actors[m].asleep && self.actors[m].ended.is_none())
            .collect();
        if alive.is_empty() || alive.iter().any(|&m| self.status(m) != Some(Status::Idle)) {
            return None;
        }
        let at = session.last_at + QUIET_AFTER;
        if pace.now < at {
            pace.at(at);
            return None;
        }
        let a = self.view.picked.filter(|p| alive.contains(p)).unwrap_or(alive[0]);
        let (text, _) = self.line(a, Section::Idle, When::Always, at as u64)?;
        Some(Moment { at, actor: a, prio: 1, view: false, quiet: true, section: Section::Idle, text })
    }
}

struct Build<'a> {
    mori: &'a Mori,
    s: usize,
    level: Level,
    now: i64,
    pace: Pace,
    /// The ground, with any holes where the floor gave way, without the blocks.
    rock: Vec<i32>,
    /// What characters stand on: the rock and the piles of blocks on it.
    land: Vec<i32>,
    /// Bodies placed so far this frame. Parents are placed before their subagents.
    bodies: HashMap<usize, Body>,
    effects: Vec<Effect>,
    stains: Vec<(i32, i32, Rgb)>,
    /// Cracks running through the floor during a death scene.
    cuts: Vec<(i32, i32)>,
    dim: bool,
    filming: Option<bool>,
    /// Characters in a death scene that dims the stage, who aren't dimmed with it.
    dying: Vec<usize>,
    speaker: Option<usize>,
}

impl<'a> Build<'a> {
    fn new(mori: &'a Mori, s: usize, level: Level, pace: Pace) -> Build<'a> {
        let rock = level.top.clone();
        Build {
            mori,
            s,
            now: pace.now,
            land: rock.clone(),
            rock,
            level,
            pace,
            bodies: HashMap::new(),
            effects: Vec::new(),
            stains: Vec::new(),
            cuts: Vec::new(),
            dim: false,
            filming: None,
            dying: Vec::new(),
            speaker: None,
        }
    }

    fn stage(&mut self) -> Stage {
        let mori = self.mori;
        let s = self.s;
        let on = mori.on_stage(s, self.level.cols, self.pace.now);
        self.holes();
        let blocks = self.blocks();
        let mut cracks = self.cracks();
        let moment = mori.moment(s, &mut self.pace);
        self.speaker = moment.as_ref().map(|m| m.actor);

        let mut actors: Vec<Actor> = on.iter().map(|&a| self.actor(a)).collect();
        let exits = actors
            .iter()
            .filter(|actor| {
                let c = &mori.actors[actor.index];
                c.alive || c.death.is_some_and(|d| d.at > self.now)
            })
            .map(|actor| {
                let c = &mori.actors[actor.index];
                let (x, bottom) = self.level.exit(c.k, c.shape.w);
                Exit { x, bottom, w: c.shape.w, h: c.shape.h, hue: c.hue, lit: actor.home }
            })
            .collect();
        if self.dim {
            for actor in actors.iter_mut().filter(|a| !self.dying.contains(&a.index)) {
                actor.fill = actor.fill.dim(0.66);
            }
        }
        let health = actors
            .iter()
            .map(|actor| {
                let c = &mori.actors[actor.index];
                c.pressure.iter().for_each(|&(at, _)| self.pace.at(at));
                let dead = c.death.is_some_and(|d| d.at <= self.now) && !c.alive;
                Health { name: c.name, hue: c.hue, left: c.health_at(self.now), dead, asleep: c.asleep_at(self.now) }
            })
            .collect();
        let narration = moment.and_then(|m| self.narration(&m, &actors));
        let title = self.title();
        cracks.append(&mut self.cuts);
        Stage {
            number: mori.sessions[s].number,
            prompt: mori.model.agent(&mori.sessions[s].root).and_then(|a| a.prompt.clone()),
            title,
            dim: self.dim,
            filming: self.filming,
            land: self.rock.clone(),
            cracks,
            stains: std::mem::take(&mut self.stains),
            blocks,
            exits,
            actors,
            health,
            effects: std::mem::take(&mut self.effects),
            narration,
        }
    }

    // ── the level as it stands now ─────────────────────────────────────────

    fn holes(&mut self) {
        let session = &self.mori.sessions[self.s];
        for hole in &session.holes {
            if hole.at > self.now {
                self.pace.at(hole.at);
                continue;
            }
            let (from, to, floor) = self.pit(&self.mori.actors[hole.actor], hole.spot);
            for x in from.max(0)..to.min(self.level.cols) {
                self.rock[x as usize] = self.rock[x as usize].max(floor);
            }
        }
        self.land.clone_from(&self.rock);
    }

    /// Where the floor gives way under `c` standing in `spot`: the columns, and the new top of
    /// the land there, deep enough to lie in. Worked out on the level itself, so it comes out the
    /// same however the blocks lie.
    fn pit(&self, c: &Character, spot: Spot) -> (i32, i32, i32) {
        let level = &self.level;
        let (w, flat) = (c.shape.w, c.shape.flat());
        let x = level.x(spot, c.k, w);
        let from = x + w / 2 - flat.w / 2 - 1;
        (from, from + flat.w + 2, (level.stand(&level.top, x, w) + 5).min(level.floor))
    }

    /// The piles become part of the land.
    fn blocks(&mut self) -> Vec<Rect> {
        let session = &self.mori.sessions[self.s];
        let level = &self.level;
        let mut piles = session.settled.clone();
        let mut flying = None;
        for push in &session.pushes {
            if push.at > self.now {
                self.pace.at(push.at);
                break;
            }
            let (from, to) = (usize::from(push.from), usize::from(push.to));
            piles[from] = piles[from].saturating_sub(1);
            if self.now < push.at + PUSH_FLY {
                flying = Some((from, to, progress(self.now - push.at, 0, PUSH_FLY)));
                self.pace.moving();
                break;
            }
            piles[to] += 1;
        }
        let b = level.block;
        let base = |slot: usize| {
            let x = level.slot(slot);
            (x.max(0)..(x + b).min(level.cols)).map(|c| self.rock[c as usize]).min().unwrap_or(level.ground)
        };
        let mut rects = Vec::new();
        for (slot, &n) in piles.iter().enumerate() {
            let (x, g) = (level.slot(slot), base(slot));
            for j in 0..i32::from(n) {
                rects.push(Rect { x, y: g - 2 * (j + 1), w: b, h: 2 });
            }
            for c in x.max(0)..(x + b).min(level.cols) {
                self.land[c as usize] = self.land[c as usize].min(g - 2 * i32::from(n));
            }
        }
        if let Some((from, to, p)) = flying {
            let p = ease_in_out(p);
            let y0 = base(from) - 2 * (i32::from(piles[from]) + 1);
            let y1 = base(to) - 2 * (i32::from(piles[to]) + 1);
            let lift = if y1 < y0 { (y0 - y1) as f32 / 2.0 + 1.0 } else { 0.0 };
            let (x0, x1) = (level.slot(from), level.slot(to));
            rects.push(Rect { x: lerp(x0, x1, p), y: lerp(y0, y1, p) - arc(p, lift), w: b, h: 2 });
        }
        rects
    }

    /// Where characters hit the invisible edge, the ground cracked: deeper each time it
    /// happens in the same place, and at least as deep as the strike. A crack runs down and
    /// back from just behind the leading edge, so an edge it's near stays an edge. Where the
    /// floor has since given way, the crack went with it.
    fn cracks(&mut self) -> Vec<(i32, i32)> {
        let session = &self.mori.sessions[self.s];
        let mut depth: BTreeMap<i32, (u32, i32)> = BTreeMap::new();
        for crack in &session.cracks {
            if crack.at > self.now {
                self.pace.at(crack.at);
                continue;
            }
            let c = &self.mori.actors[crack.actor];
            let w = c.shape.w;
            let x = self.level.x(crack.spot, c.k, w);
            let stand = self.level.stand(&self.rock, x, w);
            // Just behind its leading edge; out over a gap, the last ground it stood on.
            let dir = i32::from(crack.dir);
            let mut cx = if dir > 0 { x + w - 2 } else { x + 1 };
            while self.at(cx).is_some_and(|top| top > stand + 2) && (cx - (x + w / 2)) * dir > 0 {
                cx -= dir;
            }
            let d = depth.entry(cx).or_insert((0, dir));
            *d = ((d.0 + 1).max(crack.strike).min(3), dir);
        }
        let mut pixels = Vec::new();
        for (&cx, &(d, dir)) in &depth {
            let Some(top) = self.at(cx) else { continue };
            let shape: &[(i32, i32)] = match d {
                1 => &[(0, 0)],
                2 => &[(0, 0), (0, 1), (-1, 0)],
                _ => &[(0, 0), (0, 1), (0, 2), (-1, 0), (-1, 1), (-2, 0)],
            };
            for &(back, dy) in shape {
                let x = cx + back * dir;
                let fell = usize::try_from(x).ok().is_some_and(|i| self.rock[i] != self.level.top[i]);
                if !fell && self.at(x).is_some_and(|t| top + dy >= t) {
                    pixels.push((x, top + dy));
                }
            }
        }
        pixels
    }

    fn at(&self, x: i32) -> Option<i32> {
        usize::try_from(x).ok().and_then(|x| self.rock.get(x).copied())
    }

    // ── characters ─────────────────────────────────────────────────────────

    /// Blood: `n` drops fly out from `from` and land on the ground `e` ms after the splash, where
    /// they stay, drying dark once the scene is over (`dry`).
    fn splat(&mut self, c: &Character, from: (i32, i32), e: i64, n: u64, dry: bool) {
        if e < 0 {
            return;
        }
        let mut flying = Vec::new();
        for i in 0..n {
            let h = cast::hash(&[c.id.as_bytes(), &i.to_le_bytes()]);
            let dx = (h % 17) as i32 - 8;
            let lift = 2.0 + ((h >> 8) % 6) as f32;
            let len = 300 + i64::from(dx.abs()) * 45 + (h >> 16) as i64 % 150;
            let x = from.0 + dx;
            let Some(ground) = usize::try_from(x).ok().and_then(|i| self.land.get(i)).copied() else { continue };
            if e >= len {
                self.stains.push((x, ground, if dry { DRIED } else { BLOOD }));
            } else {
                let p = e as f32 / len as f32;
                flying.push((lerp(from.0, x, p), lerp(from.1, ground - 1, p) - arc(p, lift), BLOOD));
            }
        }
        if !flying.is_empty() {
            self.effects.push(Effect::Pixels(flying));
            self.pace.moving();
        }
    }

    fn rest(&self, c: &Character, spot: Spot) -> Body {
        let Shape { w, h } = c.shape;
        let x = self.level.x(spot, c.k, w);
        let bottom = match spot {
            Spot::Exit => self.level.exit(c.k, w).1,
            _ => self.level.stand(&self.land, x, w),
        };
        Body { x, bottom, w, h, fill: c.hue, outline: false, hidden: false }
    }

    fn actor(&mut self, a: usize) -> Actor {
        let mori = self.mori;
        let c = &mori.actors[a];
        let now = self.now;

        for beat in c.beats.iter().filter(|b| b.start > now) {
            self.pace.at(beat.start);
        }
        let beat = c.beats.iter().rev().find(|b| b.start <= now).copied();
        let mut pose = match beat {
            Some(beat) => self.play(c, a, beat, now - beat.start),
            None => {
                let body = self.rest(c, c.beats.front().map_or(c.spot, |b| b.from));
                Pose { body, epitaph: None, home: false, stood: None, scene_top: body.top(), says: None }
            }
        };
        // Once its scene is over, the dead get a little grave cross where they lie. A heap of
        // lemmings shares the first one's.
        let grave = pose.epitaph.is_some();
        if grave {
            let b = pose.body;
            let heaped = matches!(beat.map(|b| b.act), Some(Act::Die(Method::Cliff)));
            let shared = heaped && c.death.is_some_and(|d| d.nth > 0);
            pose.body = Body { x: b.x + b.w / 2 - CROSS.0 / 2, w: CROSS.0, h: CROSS.1, hidden: shared, ..b };
        }
        self.bodies.insert(a, pose.body);
        let alive = pose.epitaph.is_none() && pose.stood.is_none();

        let picked = mori.view.picked == Some(a);
        let label = match pose.epitaph {
            // A line that went together has one epitaph, the first one's.
            Some(_) if c.death.is_some_and(|d| d.nth > 0) => None,
            Some(word) => Some(Label::Epitaph(word)),
            None if pose.says.is_some() => pose.says.map(Label::Says),
            None if picked || self.speaker == Some(a) => Some(Label::Name { faint: false }),
            None => {
                let since = [mori.view.switched_at.unwrap_or(i64::MIN), c.first_at, c.noticed_at]
                    .into_iter()
                    .max()
                    .unwrap_or(i64::MIN);
                self.pace.at(since + NAME_FOR - 1000);
                self.pace.at(since + NAME_FOR);
                (self.level.names && now < since + NAME_FOR)
                    .then_some(Label::Name { faint: now >= since + NAME_FOR - 1000 })
            }
        };

        // Lines hang above where it rests; a death's above its whole scene, then above the outline.
        let above = |b: Body, top: i32| (b.x + b.w / 2, top.div_euclid(2) - 1);
        let (voice, last_words) = match pose.stood {
            Some(stood) => {
                let scene = above(stood, pose.scene_top.min(stood.top()));
                (if pose.epitaph.is_some() { above(pose.body, pose.body.top()) } else { scene }, scene)
            }
            None => {
                let rest = beat.map_or(pose.body, |b| self.rest(c, b.to));
                let voice = above(rest, rest.top());
                (voice, voice)
            }
        };

        let body = pose.body;
        Actor {
            agent: c.id.clone(),
            name: c.name,
            hue: c.hue,
            x: body.x,
            bottom: body.bottom,
            w: body.w,
            h: body.h,
            fill: body.fill,
            outline: body.outline,
            hidden: body.hidden,
            alive,
            grave,
            home: pose.home,
            label,
            picked,
            voice,
            last_words,
            index: a,
        }
    }

    fn play(&mut self, c: &Character, a: usize, beat: Beat, e: i64) -> Pose {
        let Shape { w, h } = c.shape;
        let (from, to) = (self.rest(c, beat.from), self.rest(c, beat.to));
        let mut body = to;
        let mut moving = false;
        let mut home = false;

        match beat.act {
            Act::Arrive { parent } => match parent.and_then(|p| self.bodies.get(&p).copied()) {
                None => {
                    const DROP: i64 = 450;
                    if e < DROP {
                        let p = progress(e, 0, DROP);
                        body.bottom = to.bottom - (12.0 * (1.0 - p * p)).round() as i32;
                        moving = true;
                    }
                }
                // It climbs out of the top of whoever spawned it, then hops down to its spot.
                Some(parent) => {
                    const POP: i64 = 250;
                    const DOWN: i64 = 500;
                    let (px, pb) = (parent.x + parent.w / 2 - w / 2, parent.top() - 1);
                    if e < POP {
                        let p = progress(e, 0, POP);
                        body = Body {
                            x: px,
                            bottom: pb,
                            w: ((w as f32 * p).round() as i32).max(1),
                            h: ((h as f32 * p).round() as i32).max(1),
                            ..body
                        };
                        moving = true;
                    } else if e < POP + DOWN {
                        let p = progress(e, POP, DOWN);
                        body.x = lerp(px, to.x, p);
                        body.bottom = lerp(pb, to.bottom, ease_out(p)) - arc(p, 3.0);
                        moving = true;
                    }
                }
            },
            Act::Go { wait, len } => {
                if e < wait {
                    body = from;
                    home = beat.from == Spot::Exit && e < LEAVE / 2;
                    self.pace.at(beat.start + wait);
                    if home {
                        self.pace.at(beat.start + LEAVE / 2);
                    }
                } else if e < wait + len {
                    let p = ease_in_out(progress(e, wait, len));
                    (body.x, body.bottom) = self.travel(from, to, w, p);
                    moving = true;
                }
            }
            Act::Push { from: f, to: t } => {
                let dir = (i32::from(t) - i32::from(f)).signum();
                if e < PUSH_LEAN {
                    body = Body { x: from.x + dir, ..from };
                    moving = true;
                } else if e < PUSH_LEAN + PUSH_FLY {
                    let p = ease_in_out(progress(e, PUSH_LEAN, PUSH_FLY));
                    body.x = lerp(from.x + dir, to.x, p);
                    body.bottom = self.level.stand(&self.land, body.x, w);
                    moving = true;
                }
            }
            Act::Bump { dir } => {
                const FLASH: i64 = 200;
                const KNOCK: i64 = 250;
                let dir = i32::from(dir);
                let contact = to.x + 3 * dir;
                // Toward a drop or the gap, it's a leap that meets the edge in the air.
                let lead = if dir > 0 { contact + w - 1 } else { contact };
                let leap =
                    usize::try_from(lead).ok().and_then(|x| self.land.get(x)).is_some_and(|&t| t > to.bottom + 2);
                let lift = i32::from(leap);
                let flash = Effect::Flash {
                    x: if dir > 0 { contact + w } else { contact - 1 },
                    top: to.top() - 1 - lift,
                    bottom: to.bottom - lift,
                };
                // The hero presses the edge a second time before giving way.
                let again = if c.who == Personality::Hero { 350 } else { 0 };
                let knock = PRESS + FLASH + again;
                moving = e < knock + KNOCK;
                if e < PRESS {
                    let p = progress(e, 0, PRESS);
                    body.x = lerp(to.x, contact, p);
                    body.bottom = to.bottom - if leap { lerp(0, 2, p) } else { 0 };
                } else if e < PRESS + FLASH {
                    body.x = contact;
                    body.bottom = to.bottom - lift;
                    self.effects.push(flash);
                } else if e < PRESS + FLASH + 100 {
                    body.x = contact - 2 * dir;
                } else if e < PRESS + FLASH + 200 {
                    body.x = lerp(contact - 2 * dir, contact, progress(e, PRESS + FLASH + 100, 100));
                } else if e < knock {
                    body.x = contact;
                    body.bottom = to.bottom - lift;
                    self.effects.push(flash);
                } else if moving {
                    body.x = lerp(contact, to.x, ease_out(progress(e, knock, KNOCK)));
                }
            }
            Act::Turn => {}
            Act::Home => {
                home = e >= 150;
                self.pace.at(beat.start + 150);
            }
            // It stretches up, and a subagent climbs out of the top.
            Act::Split => {
                if e < SPLIT {
                    body.h = h + if (100..SPLIT - 100).contains(&e) { 2 } else { 1 };
                    moving = true;
                }
            }
            // Slower than a death, and in its own colour throughout.
            Act::Sleep | Act::Wake => {
                let len = beat.act.lock(c.who);
                let p = ease_in_out(progress(e, 0, len));
                let p = if beat.act == Act::Wake { 1.0 - p } else { p };
                let flat = c.shape.flat();
                body.w = lerp(w, flat.w, p);
                body.h = lerp(h, flat.h, p);
                body.x = to.x + w / 2 - body.w / 2;
                body.fill = c.hue.mix(c.hue.dim(ASLEEP), p);
                moving = e < len;
                home = beat.act == Act::Wake && e >= len && beat.to == Spot::Exit;
            }
            Act::Rise => {
                const RISE: i64 = 500;
                if e < RISE {
                    let p = progress(e, 0, RISE);
                    let flat = c.shape.flat();
                    body.w = lerp(flat.w, w, p);
                    body.h = lerp(flat.h, h, p);
                    body.fill = DEAD.mix(c.hue, p);
                    body.outline = p < 0.5;
                    moving = true;
                }
            }
            Act::Die(method) => return self.die(c, a, method, e, to, beat.to),
        }
        if moving {
            self.pace.moving();
        }
        Pose { body, epitaph: None, home, stood: None, scene_top: body.top(), says: None }
    }

    fn travel(&self, from: Body, to: Body, w: i32, p: f32) -> (i32, i32) {
        let level = &self.level;
        let feet = |x: i32| level.stand(&self.land, x, w);
        let mut legs = Vec::new();
        // An exit floating above the plateau is a jump down at the start, or up at the end.
        if from.bottom != feet(from.x) {
            legs.push(Leg::Jump { from: (from.x, from.bottom), to: (from.x, feet(from.x)), arc: 1 });
        }
        legs.extend(level.route(&self.land, from.x, to.x, w));
        if to.bottom != feet(to.x) {
            legs.push(Leg::Jump { from: (to.x, feet(to.x)), to: (to.x, to.bottom), arc: 2 });
        }
        let total: f32 = legs.iter().map(|l| l.cost()).sum();
        let mut u = p * total;
        for leg in &legs {
            let cost = leg.cost();
            if u <= cost && cost > 0.0 {
                let q = u / cost;
                return match *leg {
                    Leg::Walk { from, to } => {
                        let x = lerp(from, to, q);
                        (x, feet(x))
                    }
                    Leg::Jump { from, to, arc: height } => {
                        (lerp(from.0, to.0, q), lerp(from.1, to.1, q) - arc(q, height as f32))
                    }
                };
            }
            u -= cost;
        }
        (to.x, to.bottom)
    }

    fn die(&mut self, c: &Character, a: usize, method: Method, e: i64, stood: Body, spot: Spot) -> Pose {
        let Shape { w, h } = c.shape;
        // Where it stood before any floor went.
        let stood = Body { bottom: self.level.stand(&self.level.top, stood.x, w).min(stood.bottom), ..stood };
        let bottom = stood.bottom;
        let cols = self.level.cols;
        let tallest = match method {
            Method::Scythe => GRIM.h(),
            Method::Blade => GUILLOTINE.h(),
            Method::Wolves => h.max(8),
            Method::Cliff => h + 5,
            Method::Floor => h,
            Method::Bunny => h.max(5) + 1,
            // Held up a pixel clear of its head, and the way it drifts down to there.
            Method::Grenade => h + 6 + GRENADE.h() + PIN.h(),
            Method::Enlightened => h + 6,
            Method::Saucer => h + 10,
        };
        let scene_top = bottom - tallest + 1;
        let done = |body: Body| Pose {
            body,
            epitaph: Some(method.epitaph()),
            home: false,
            stood: Some(stood),
            scene_top,
            says: None,
        };
        let playing = |body: Body| Pose { body, epitaph: None, home: false, stood: Some(stood), scene_top, says: None };
        let mut body = stood;
        let mut says = None;

        match method {
            // The lights go down, the reaper fades in beside it, checks a clipboard, taps it
            // once with the scythe, nods, and goes. The only time reaper shows itself.
            Method::Scythe => {
                if e >= 2500 {
                    return done(stood.laid(c.shape));
                }
                self.dim = true;
                let fill = match e {
                    ..300 => 0,
                    300..800 => (1 + (e - 300) / 167).min(3) as u8,
                    800..2100 => 3,
                    _ => (2 - (e - 2100) / 134).max(0) as u8,
                };
                let rx = stood.x + w + 1;
                let nudge = if (1300..1400).contains(&e) { -1 } else { 0 };
                let nod = i32::from((1900..2100).contains(&e));
                if fill > 0 {
                    let sprite = Effect::Sprite {
                        sprite: &GRIM,
                        x: rx + nudge,
                        bottom: bottom + nod,
                        flip: false,
                        fill,
                        clip: bottom,
                    };
                    self.effects.push(sprite);
                }
                if (900..1300).contains(&e) {
                    let top = bottom - GRIM.h() + 1;
                    self.effects.push(Effect::Pixels(vec![(rx + 6, top + 6, WHITE), (rx + 6, top + 7, WHITE)]));
                }
                if e < 1300 {
                    body.fill = c.hue.dim(0.66);
                } else {
                    self.dying.push(a);
                }
                if (1300..1360).contains(&e) {
                    body.fill = WHITE;
                } else if e >= 1360 {
                    body.fill = c.hue.mix(DEAD, progress(e, 1360, 540));
                    if e >= 1600 {
                        let flat = stood.laid(c.shape);
                        let p = progress(e, 1600, 300);
                        body = Body {
                            x: lerp(stood.x, flat.x, p),
                            w: lerp(w, flat.w, p),
                            h: lerp(h, flat.h, p),
                            outline: e >= 1900,
                            ..body
                        };
                    }
                }
            }
            // A pack runs in, a dust cloud swallows everything, and the wolves trot off.
            Method::Wolves => {
                self.splat(c, (stood.x + w / 2, bottom - h / 2), e - 900, 14, e >= 2800);
                if e >= 2800 {
                    return done(stood.laid(c.shape));
                }
                let n = 2 + (cast::seed(&c.id, 7) % 2) as i32;
                let gather = |i: i32| stood.x + w + 1 + i * 6;
                let away = |i: i32| cols + 4 + i * 12;
                let wolf = |frame: i64, x: i32, flip: bool| Effect::Sprite {
                    sprite: &WOLF[(frame % 2) as usize],
                    x,
                    bottom,
                    flip,
                    fill: 3,
                    clip: bottom,
                };
                if e < 800 {
                    for i in 0..n {
                        self.effects.push(wolf(e / 100, lerp(away(i), gather(i), progress(e, 0, 800)), false));
                    }
                } else if e < 2000 {
                    body.hidden = true;
                    let frame = e / 80;
                    let (x0, x1) = (stood.x - 3, stood.x + w + 8);
                    let mut pixels = Vec::new();
                    for y in bottom - 6..=bottom {
                        for x in x0..=x1 {
                            if cast::hash(&[&x.to_le_bytes(), &y.to_le_bytes(), &frame.to_le_bytes()]) % 100 < 55 {
                                pixels.push((x, y, dust()));
                            }
                        }
                    }
                    // Bits poke out: a tail, a paw, a pixel of whoever is in there.
                    let k = frame as i32;
                    let fur = |key| sprites::ink(key).expect("fur is in the palette");
                    pixels.push((x1 + 1, bottom - 2 - k % 3, fur(b'f')));
                    pixels.push((x0 - 1, bottom - k % 2, fur(b'F')));
                    pixels.push((x0 + (k * 3).rem_euclid(x1 - x0 + 1), bottom - 7, c.hue));
                    self.effects.push(Effect::Pixels(pixels));
                } else {
                    body = stood.laid(c.shape);
                    let trot = e >= 2200;
                    for i in 0..n {
                        let x = if trot { lerp(gather(i), away(i), progress(e, 2200, 600)) } else { gather(i) };
                        self.effects.push(wolf(if trot { e / 150 } else { 0 }, x, trot));
                        if trot && i == n - 1 {
                            self.effects.push(Effect::Pixels(vec![(x + 10, bottom - 2, c.hue)]));
                        }
                    }
                }
            }
            // The frame rises, it looks up, the blade drops, a little comes off the top and
            // bounces into the basket, and the frame sinks away again.
            Method::Blade => {
                let cut = 2.min(h - 1);
                let short = Shape { w, h: h - cut };
                self.splat(c, (stood.x + w / 2, stood.top() + cut), e - 920, 10, e >= 2300);
                if e >= 2300 {
                    return done(Body { h: h - cut, ..stood }.laid(short));
                }
                let x0 = stood.x + w / 2 - 4;
                let sink = match e {
                    ..300 => 16 - 16 * e / 300,
                    300..1800 => 0,
                    1800..2100 => 16 * (e - 1800) / 300,
                    _ => 16,
                } as i32;
                let frame_top = bottom - GUILLOTINE.h() + 1 + sink;
                let top = stood.top();
                let blade = match e {
                    ..800 => frame_top + 2,
                    800..920 => lerp(frame_top + 2, top + cut - 2 + sink, progress(e, 800, 120)),
                    _ => top + cut - 2 + sink,
                };
                if sink < 16 {
                    let clip = bottom;
                    self.effects.push(Effect::Sprite {
                        sprite: &GUILLOTINE,
                        x: x0,
                        bottom: bottom + sink,
                        flip: false,
                        fill: 3,
                        clip,
                    });
                    self.effects.push(Effect::Sprite {
                        sprite: &BLADE,
                        x: x0 + 2,
                        bottom: blade + 1,
                        flip: false,
                        fill: 3,
                        clip,
                    });
                }
                let basket_x = x0 + GUILLOTINE.w() + 1;
                let fill = if e < 2100 { 3 } else { (2 - (e - 2100) / 67).max(0) as u8 };
                if fill > 0 {
                    self.effects.push(Effect::Sprite {
                        sprite: &BASKET,
                        x: basket_x,
                        bottom,
                        flip: false,
                        fill,
                        clip: bottom,
                    });
                    if e >= 900 {
                        let (rest_x, rest_bottom) = (basket_x + 2 - w / 2, bottom - 2);
                        let (x, piece_bottom) = match e {
                            ..1300 => {
                                let p = progress(e, 900, 400);
                                (lerp(stood.x, rest_x, p), lerp(top + cut - 1, rest_bottom, p) - arc(p, 4.0))
                            }
                            1300..1500 => (rest_x, rest_bottom - arc(progress(e, 1300, 200), 1.0)),
                            _ => (rest_x, rest_bottom),
                        };
                        let piece = (0..w).flat_map(|dx| (0..cut).map(move |dy| (x + dx, piece_bottom - dy, c.hue)));
                        self.effects.push(Effect::Pixels(piece.collect()));
                    }
                }
                if (300..800).contains(&e) {
                    body.h = h + 1;
                } else if e >= 900 {
                    body.h = h - cut;
                }
                if e >= 1800 {
                    body = Body { h: h - cut, ..stood }.laid(short);
                } else if e >= 1500 {
                    body.fill = c.hue.mix(DEAD, progress(e, 1500, 300));
                }
            }
            // The documentary. It's being filmed: they migrate, as the narration will put it, to
            // the edge, where they stop, because lemmings don't jump. Something just out of shot
            // sees to the rest. They go into the water at the bottom.
            Method::Cliff => {
                let (g0, g1) = self.level.gap();
                let near = stood.x < g1;
                let (brink_x, dir) = if near { (g0 - w, 1) } else { (g1, -1) };
                let brink = Body { x: brink_x, bottom: self.level.stand(&self.land, brink_x, w), ..stood };
                let flat = c.shape.flat();
                let nth = c.death.map_or(0, |d| i32::from(d.nth));
                let lx = (g0 + (g1 - g0 - flat.w) / 2).max(g0.min(g1 - flat.w)) + [0, 1, -1][(nth % 3) as usize];
                let heap = self.level.stand(&self.land, lx, flat.w) - nth * flat.h;
                let lie = Body { x: lx, bottom: heap, ..stood.laid(c.shape) };
                let over = brink_x + dir * (w / 2 + 2);
                let walk = (i64::from((brink_x - stood.x).abs()) * 30).min(1400);
                let pose = if c.who == Personality::ShowOff { 250 } else { 0 };
                let fall_for = falling(lie.bottom - brink.bottom);
                // Marched to the edge; stopped; a pose for the camera, perhaps; then the shove.
                let t1 = 150 + walk;
                let t3 = t1 + 750 + pose;
                let (t4, t5) = (t3 + 160, t3 + 160 + fall_for);
                if e >= t5 + 400 {
                    return done(lie);
                }
                self.filming = Some(self.filming.unwrap_or(false) || e % 1000 < 650);
                match e {
                    // It turns to face the gap.
                    ..150 => body.w = (w - 1).max(1),
                    _ if e < t1 => (body.x, body.bottom) = self.travel(stood, brink, w, progress(e, 150, walk)),
                    _ if e < t3 => {
                        // At the edge, it stops, and looks down. Lemmings don't jump.
                        body = brink;
                        if pose > 0 && (t1 + 150..t1 + 150 + pose).contains(&e) {
                            body.bottom -= arc(progress(e, t1 + 150, pose), 2.0);
                        } else if e >= t1 + 200 {
                            body.h = (h - 1).max(1);
                        }
                    }
                    // Shoved from behind, from out of shot: a lurch forward, stiff, over the edge.
                    _ if e < t4 => {
                        let p = progress(e, t3, t4 - t3);
                        body = Body { x: lerp(brink_x, over, p * p), w: w + 1, h: (h - 1).max(1), ..brink };
                    }
                    _ if e < t5 => {
                        let p = progress(e, t4, fall_for);
                        body = Body { x: over, bottom: lerp(brink.bottom, lie.bottom, p * p), ..brink };
                    }
                    _ => {
                        body = lie;
                        let t = progress(e, t5, 400);
                        let splash = (0..8)
                            .map(|i: i32| {
                                let dx = i - 4 + i32::from(i >= 4);
                                (lie.x + lie.w / 2 + dx, lie.bottom - arc(t, 2.0 + (i % 3) as f32), WATER)
                            })
                            .collect();
                        self.effects.push(Effect::Pixels(splash));
                    }
                }
            }
            // A sweet little rabbit hops up and sits beside it. Then it isn't sweet at all.
            Method::Bunny => {
                const HOP: i64 = 900;
                const SIT: i64 = 1700;
                const POUNCE: i64 = 1850;
                const MESS: i64 = 2850;
                const AWAY: i64 = 3200;
                const GONE: i64 = 3600;
                let (bx, bw) = (stood.x + w + 1, BUNNY.w());
                self.splat(c, (stood.x + w / 2, bottom - h / 2), e - POUNCE, 24, e >= GONE);
                if e >= GONE {
                    return done(stood.laid(c.shape));
                }
                let hop = |t: i64| arc((t % 300) as f32 / 300.0, 2.0);
                let bunny = match e {
                    ..HOP => Some((lerp(cols + 1, bx, progress(e, 0, HOP)), bottom - hop(e))),
                    _ if e < SIT => Some((bx, bottom)),
                    _ if e < POUNCE => {
                        let p = progress(e, SIT, POUNCE - SIT);
                        Some((lerp(bx, stood.x, p), bottom - arc(p, 3.0)))
                    }
                    _ if e < MESS => None,
                    _ if e < AWAY => Some((bx, bottom)),
                    _ => Some((lerp(bx, cols + 1, progress(e, AWAY, GONE - AWAY)), bottom - hop(e))),
                };
                if let Some((x, b)) = bunny {
                    self.effects.push(Effect::Sprite {
                        sprite: &BUNNY,
                        x,
                        bottom: b,
                        flip: e >= AWAY,
                        fill: 3,
                        clip: b,
                    });
                    if e >= MESS {
                        let mouth = if e >= AWAY { x + bw - 1 } else { x };
                        self.effects.push(Effect::Pixels(vec![(mouth, b - 1, BLOOD), (mouth, b, BLOOD)]));
                    }
                }
                if (POUNCE..MESS).contains(&e) {
                    body.hidden = true;
                    let frame = e / 70;
                    let fur = sprites::ink(b'E').expect("fur is in the palette");
                    let mut fray = Vec::new();
                    for y in stood.top() - 2..=bottom {
                        for x in stood.x - 2..stood.x + w + 3 {
                            match cast::hash(&[&x.to_le_bytes(), &y.to_le_bytes(), &frame.to_le_bytes()]) % 100 {
                                0..30 => fray.push((x, y, fur)),
                                30..48 => fray.push((x, y, BLOOD)),
                                48..56 => fray.push((x, y, c.hue)),
                                _ => {}
                            }
                        }
                    }
                    self.effects.push(Effect::Pixels(fray));
                } else if e >= MESS {
                    body = stood.laid(c.shape);
                }
            }
            // The holy hand grenade. A golden orb with a little cross on top appears over its head
            // and drifts down to it. It holds it up, takes out the pin, and counts to three, as
            // instructed: one, two, five.
            Method::Grenade => {
                const DOWN: i64 = 1200;
                const PULL: i64 = 1800;
                const COUNT: [(i64, &str); 3] = [(2300, "one!"), (3000, "two!"), (3700, "five!")];
                const BOOM: i64 = 4300;
                const SETTLE: i64 = 5300;
                const DRIFT: i32 = 4;
                let gx = stood.x + w / 2 - GRENADE.w() / 2;
                // The orb's bottom row once it's held up, a pixel clear of its raised head.
                let held = stood.top() - 3;
                let (ox, oy) = (gx + GRENADE.w() / 2, held - GRENADE.h() / 2);
                self.splat(c, (ox, oy), e - BOOM, 26, e >= SETTLE);
                if e >= SETTLE {
                    return done(stood.laid(c.shape));
                }
                // Each number hangs over it until the next, and the last one until the smoke.
                if e < BOOM + 400 {
                    says = COUNT.iter().rev().find(|&&(t, _)| e >= t).map(|&(_, word)| word);
                }
                let fill = (1 + e / 100).min(3) as u8;
                if e < BOOM {
                    // It drifts down to just over its head, which reaches up to hold it there, and
                    // dips it once for each number.
                    let counting = COUNT.iter().any(|&(t, _)| (t..t + 120).contains(&e));
                    let b = match e {
                        ..DOWN => lerp(held - DRIFT, held, ease_out(progress(e, 0, DOWN))),
                        _ if counting => held + 1,
                        _ => held,
                    };
                    if e >= DOWN {
                        body.h = if counting { h } else { h + 1 };
                    }
                    self.effects.push(Effect::Sprite {
                        sprite: &GRENADE,
                        x: gx,
                        bottom: b,
                        flip: false,
                        fill,
                        clip: bottom,
                    });
                    // The pin: out, up, and tossed down beside it, where it lies until the end.
                    let px = gx + GRENADE.w() / 2 - PIN.w() / 2;
                    let lx = (px + if stood.x < cols / 2 { 5 } else { -5 }).clamp(0, (cols - PIN.w()).max(0));
                    let ly = self.land.get(usize::try_from(lx + 1).unwrap_or(0)).copied().unwrap_or(bottom + 1) - 1;
                    let pulled = held - GRENADE.h() - 2;
                    let pin = match e {
                        ..PULL => (px, b - GRENADE.h()),
                        _ if e < PULL + 150 => (px, held - GRENADE.h() - lerp(0, 2, progress(e, PULL, 150))),
                        _ => {
                            let p = progress(e, PULL + 150, 450);
                            (lerp(px, lx, p), lerp(pulled, ly, p) - arc(p, 4.0))
                        }
                    };
                    self.effects.push(Effect::Sprite {
                        sprite: &PIN,
                        x: pin.0,
                        bottom: pin.1,
                        flip: false,
                        fill,
                        clip: i32::MAX,
                    });
                    if e < PULL && e % 300 < 150 {
                        let glints = vec![(gx - 1, b - GRENADE.h() - 1, WHITE), (gx + GRENADE.w(), b - 1, WHITE)];
                        self.effects.push(Effect::Pixels(glints));
                    }
                } else if e < BOOM + 400 {
                    // A flash, then rays of white and gold, and the pin with them.
                    body.hidden = true;
                    let r = 2 + ((e - BOOM) / 40) as i32;
                    let mut burst = Vec::new();
                    if e < BOOM + 120 {
                        for dy in -3..=3 {
                            for dx in -3..=3 {
                                if dx * dx + dy * dy <= 10 {
                                    burst.push((ox + dx, oy + dy, WHITE));
                                }
                            }
                        }
                    }
                    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, 1), (1, -1), (-1, -1)] {
                        for k in (r - 3).max(0)..=r {
                            burst.push((ox + dx * k, oy + dy * k, if k == r { HALO } else { WHITE }));
                        }
                    }
                    self.effects.push(Effect::Pixels(burst));
                } else {
                    // What's left comes down through the smoke.
                    let p = progress(e, BOOM + 400, 400);
                    body = Body { bottom: lerp(bottom - 8, bottom, p * p), ..stood.laid(c.shape) };
                    let rising = ((e - BOOM) / 200) as i32;
                    self.effects.push(puff(stood.x - 3, stood.x + w + 3, stood.top() + 1 - rising, e));
                }
            }
            // It understands, all at once, that it is only a rectangle. A halo, a glow, and its
            // colour leaves it, upwards, a mote at a time. What's left lies down.
            Method::Enlightened => {
                const GLOW: i64 = 600;
                const RISE: i64 = 1400;
                const DRAIN: i64 = 2400;
                const DOWN: i64 = 2700;
                const LIE: i64 = 3000;
                if e >= LIE {
                    return done(stood.laid(c.shape));
                }
                let up = match e {
                    ..GLOW => 0,
                    _ if e < RISE => lerp(0, 3, progress(e, GLOW, RISE - GLOW)),
                    _ if e < DRAIN => 3,
                    _ => lerp(3, 0, ease_in_out(progress(e, DRAIN, DOWN - DRAIN))),
                };
                body.bottom = bottom - up;
                body.fill = match e {
                    ..GLOW => c.hue,
                    _ if e < RISE => c.hue.mix(WHITE, progress(e, GLOW, RISE - GLOW)),
                    _ => WHITE.mix(DEAD, progress(e, RISE, DRAIN - RISE)),
                };
                if (300..DRAIN).contains(&e) {
                    let top = body.top();
                    let mut halo: Vec<_> = (stood.x..stood.x + w).map(|x| (x, top - 3, HALO)).collect();
                    halo.extend([(stood.x - 1, top - 2, HALO), (stood.x + w, top - 2, HALO)]);
                    self.effects.push(Effect::Pixels(halo));
                }
                if (RISE..DRAIN + 600).contains(&e) {
                    let t = e - RISE;
                    let top = body.top();
                    let motes = (0..12)
                        .filter_map(|i: i32| {
                            let rise = (t - i64::from(i) * 60) / 45;
                            (rise > 0).then(|| {
                                let x = stood.x + (i * 7) % w.max(1) + ((rise / 4) % 2) as i32;
                                (x, top + i % h.max(1) - rise as i32, c.hue.mix(WHITE, (rise as f32 / 30.0).min(0.9)))
                            })
                        })
                        .filter(|&(_, y, _)| y >= 2)
                        .collect();
                    self.effects.push(Effect::Pixels(motes));
                }
                if e >= DOWN {
                    let p = progress(e, DOWN, LIE - DOWN);
                    let flat = stood.laid(c.shape);
                    body = Body {
                        x: lerp(stood.x, flat.x, p),
                        w: lerp(w, flat.w, p),
                        h: lerp(h, flat.h, p),
                        fill: DEAD,
                        ..body
                    };
                }
            }
            // Visitors. A saucer comes over, beams it up, has a look, and sends it back down,
            // most of it. With several at once, one saucer takes them in turn.
            Method::Saucer => {
                const IN: i64 = 600;
                const UP0: i64 = 800;
                const UP1: i64 = 1600;
                const DROP: i64 = 2300;
                let sw = SAUCER.w();
                let hover = |b: Body| (b.x + b.w / 2 - sw / 2, (b.top() - 9).max(4));
                let (tx, sy) = hover(stood);
                let lie = Body { bottom, ..stood.laid(c.shape) };
                let landed = DROP + falling(bottom - sy);
                self.splat(c, (lie.x + lie.w / 2, bottom - 1), e - landed, 12, e >= landed + 700);
                if e >= landed {
                    // Not all of it came back in one piece.
                    self.stains.extend([(lie.x - 2, bottom, DEAD), (lie.x + lie.w + 1, bottom, DEAD)]);
                }
                if e >= landed + 700 {
                    return done(lie);
                }
                // Who was taken just before, and who is next: one saucer goes from one to the next.
                let session = &self.mori.sessions[self.s];
                let (me, nth) = c.death.map_or((0, 0), |d| (d.at, d.nth));
                let neighbour = |n: u8, before: bool| {
                    session.members.iter().map(|&m| &self.mori.actors[m]).filter(|o| o.id != c.id).find(|o| {
                        o.death.is_some_and(|d| d.nth == n && (d.at < me) == before)
                            && matches!(o.beats.back(), Some(Beat { act: Act::Die(Method::Saucer), .. }))
                    })
                };
                let prev = nth.checked_sub(1).and_then(|n| neighbour(n, true));
                let next = neighbour(nth + 1, false).and_then(|o| o.death).map(|d| d.at - me);
                let from = prev.map_or((-sw, 4), |p| hover(self.rest(p, p.spot)));
                let saucer = match e {
                    _ if next.is_some_and(|n| e >= n) => None,
                    ..IN => {
                        let p = ease_out(progress(e, 0, IN));
                        Some((lerp(from.0, tx, p), lerp(from.1, sy, p)))
                    }
                    _ if e < UP1 => Some((tx, sy)),
                    _ if e < DROP => Some((tx + [0, 1, 0, -1][((e / 100) % 4) as usize], sy)),
                    _ if e < DROP + 500 => Some((lerp(tx, cols + 4, progress(e, DROP, 500).powi(2)), sy)),
                    _ => None,
                };
                if let Some((x, b)) = saucer {
                    self.effects.push(Effect::Sprite {
                        sprite: &SAUCER,
                        x,
                        bottom: b,
                        flip: false,
                        fill: 3,
                        clip: bottom,
                    });
                }
                if (IN..UP1).contains(&e) {
                    let beam = (sy + 1..=bottom)
                        .flat_map(|y| (stood.x - 1..=stood.x + w).map(move |x| (x, y)))
                        .filter(|&(x, y)| (x + y) % 2 == 0)
                        .map(|(x, y)| (x, y, BEAM))
                        .collect();
                    self.effects.push(Effect::Pixels(beam));
                }
                match e {
                    _ if e < UP0 => {}
                    _ if e < UP1 => {
                        body.bottom = lerp(bottom, sy + h, ease_in_out(progress(e, UP0, UP1 - UP0)));
                        body.hidden = body.top() <= sy + 1;
                    }
                    _ if e < DROP => body.hidden = true,
                    _ if e < landed => {
                        let p = progress(e, DROP, landed - DROP);
                        body = Body { bottom: lerp(sy + 2, bottom, p * p), ..lie };
                    }
                    _ => {
                        body = lie;
                        if e < landed + 250 {
                            self.effects.push(puff(lie.x - 2, lie.x + lie.w + 2, bottom + 1, e));
                        }
                    }
                }
            }
            // It stood on the same cracked spot once too often: the cracks run, the floor goes,
            // and for a moment it stands on nothing.
            Method::Floor => {
                let (from, to, floor) = self.pit(c, spot);
                let flat = c.shape.flat();
                let lie =
                    Body { x: from + 1, bottom: self.level.stand(&self.land, from + 1, flat.w), ..stood.laid(c.shape) };
                let fall_for = falling(lie.bottom - bottom);
                let (t1, t2) = (GIVE + 300, GIVE + 300 + fall_for);
                self.splat(c, (lie.x + lie.w / 2, lie.bottom - 1), e - t2, 8, e >= t2 + 250);
                if e >= t2 + 250 {
                    return done(lie);
                }
                if e < GIVE {
                    // The cracks run out from under it, across the floor that's about to go.
                    let mid = stood.x + w / 2;
                    let reach = lerp(0, (to - from) / 2 + 1, progress(e, 0, GIVE));
                    for x in (mid - reach).max(from)..=(mid + reach).min(to - 1) {
                        if let Some(top) = self.at(x).filter(|&top| top <= bottom + 3) {
                            self.cuts.push((x, top));
                            if (x - mid).abs() <= reach / 2 {
                                self.cuts.push((x, top + 1));
                            }
                        }
                    }
                } else if e < GIVE + 250 {
                    self.effects.push(puff(from, to, floor, e));
                }
                match e {
                    _ if e < GIVE => {}
                    // On nothing at all. It looks down.
                    _ if e < t1 => {
                        if e >= GIVE + 120 {
                            body.h = (h - 1).max(1);
                        }
                    }
                    _ if e < t2 => {
                        let p = progress(e, t1, fall_for);
                        body.bottom = bottom + ((lie.bottom - bottom) as f32 * p * p).round() as i32;
                    }
                    _ => {
                        body = lie;
                        self.effects.push(puff(lie.x - 1, lie.x + lie.w + 1, lie.bottom, e));
                    }
                }
            }
        }
        self.pace.moving();
        Pose { says, ..playing(body) }
    }

    // ── words ──────────────────────────────────────────────────────────────

    fn title(&mut self) -> Option<Title> {
        let at = self.mori.view.switched_at?;
        let e = self.now - at;
        for t in [at, at + 300, at + 1900, at + TITLE_FOR] {
            self.pace.at(t);
        }
        if !(0..TITLE_FOR).contains(&e) {
            return None;
        }
        let session = &self.mori.sessions[self.s];
        let lead = self.mori.actors[*session.members.first()?].name;
        let n = session.number;
        let here = session.members.iter().filter(|&&m| self.mori.here(m, self.now)).count();
        let text = match (session.rehearsal, here) {
            (Some(method), _) => format!("rehearsal · {}", method.epitaph()),
            (None, 1) => format!("{n} · {lead}"),
            (None, _) => format!("{n} · {lead} and company"),
        };
        Some(Title { text, faint: !(300..1900).contains(&e) })
    }

    fn narration(&mut self, m: &Moment, actors: &[Actor]) -> Option<Narration> {
        let actor = actors.iter().find(|a| a.index == m.actor)?;
        // Deaths are the only lines said at the top priority.
        let death = m.prio == 5;
        let (x, row) = if death { actor.last_words } else { actor.voice };
        let (len, e) = (m.text.chars().count(), self.now - m.at);
        let (typed, held) = (typing(&m.text), typing(&m.text) + hold(&m.text));
        let shown = if e >= typed {
            len
        } else {
            self.pace.moving();
            (len as i64 * e / typed) as usize
        };
        if m.lasts() {
            return Some(Narration { text: m.text.clone(), shown, faint: 0, x, row });
        }
        let end = m.at + life(&m.text);
        let faint = if e < held {
            0
        } else if e < held + FADE / 2 {
            1
        } else {
            2
        };
        for t in [m.at + held, m.at + held + FADE / 2, end] {
            self.pace.at(t);
        }
        Some(Narration { text: m.text.clone(), shown, faint, x, row })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::parse_line;

    const SIZE: (u16, u16) = (80, 22);
    const GONE_TEST: i64 = super::super::GONE;
    const DEPART_TEST: i64 = super::super::DEPART;

    fn mori(lines: &[String]) -> Mori {
        let mut mori = Mori::new();
        mori.push(lines.iter().filter_map(|l| parse_line(l)));
        mori.advance(i64::MAX);
        mori
    }

    fn event(at: i64, agent: &str, kind: &str, fields: &str) -> String {
        let fields = if fields.is_empty() { String::new() } else { format!(",{fields}") };
        format!(r#"{{"at":{at},"host":"claude-code","agent":"{agent}","type":"{kind}"{fields}}}"#)
    }

    fn spawn(at: i64, agent: &str, model: &str) -> String {
        event(at, agent, "agent.spawned", &format!(r#""dir":"/w","model":"{model}""#))
    }

    fn sub(at: i64, agent: &str, parent: &str) -> String {
        event(at, agent, "agent.spawned", &format!(r#""dir":"/w","parent":"{parent}","label":"Explore""#))
    }

    fn acted(at: i64, agent: &str, tool: &str) -> String {
        event(at, agent, "agent.acted", &format!(r#""tool":"{tool}""#))
    }

    fn killed(at: i64, agent: &str, tier: &str, strike: u32, last: bool, command: &str) -> String {
        let attempt = serde_json::to_string(&serde_json::json!({ "command": command })).unwrap();
        event(
            at,
            agent,
            "agent.killed",
            &format!(
                r#""tool":"bash","rule":"r","tier":"{tier}","strike":{strike},"final":{last},"attempt":{}"#,
                serde_json::to_string(&attempt).unwrap()
            ),
        )
    }

    fn died(at: i64, agent: &str, rule: &str, reason: &str) -> String {
        event(at, agent, "agent.died", &format!(r#""rule":"{rule}","reason":"{reason}""#))
    }

    fn stage(mori: &Mori, now: i64) -> Stage {
        mori.scene(now, SIZE.0, SIZE.1).stage.expect("a session is followed")
    }

    fn level(mori: &Mori) -> Level {
        Level::new(&mori.sessions[0].plan, SIZE.0, SIZE.1)
    }

    #[test]
    fn the_same_events_give_the_same_mori() {
        let log = [
            spawn(0, "a", "claude-opus-5"),
            acted(500, "a", "Read"),
            acted(1500, "a", "Edit"),
            killed(3000, "a", "pattern", 1, false, "rm -rf /"),
        ];
        for t in [0, 200, 700, 1100, 1700, 3300, 5000] {
            let (one, two) = (mori(&log).scene(t, 80, 22), mori(&log).scene(t, 80, 22));
            assert_eq!(format!("{one:?}"), format!("{two:?}"), "at {t}");
        }
    }

    #[test]
    fn a_command_is_a_jump_across_the_gap() {
        let a = mori(&[spawn(0, "a", "claude-opus-5"), acted(1000, "a", "Bash")]);
        let level = level(&a);
        let actor = |t| stage(&a, t).actors[0].clone();
        assert!(actor(100).bottom < level.stand(&level.top, actor(100).x, 3), "still dropping in");
        assert_eq!((actor(900).w, actor(900).h), (3, 6), "opus stands tall");
        let (g0, g1) = level.gap();
        let frames: Vec<Actor> = (1000..4000).step_by(25).map(actor).collect();
        let over = frames.iter().find(|f| f.x >= g0 - 1 && f.x + f.w <= g1 + 1).expect("passes over the gap");
        assert!(over.bottom < level.stand(&level.top, g0 - 3, 3), "in the air: {over:?}");
        assert!(frames.windows(2).all(|f| f[1].x >= f[0].x), "heading one way");
        assert_eq!(actor(5000).x, level.x(Spot::Landing, 0, 3), "across");
        assert_eq!(a.scene(1500, 80, 22).next, Some(1500 + FRAME), "redraws while it moves");
        assert_ne!(a.scene(5000, 80, 22).next, Some(5000 + FRAME), "and rests once it lands");
    }

    #[test]
    fn reading_climbs_among_the_shelves_and_editing_moves_blocks_that_stay_moved() {
        let a = mori(&[
            spawn(0, "a", "m"),
            acted(1000, "a", "Read"),
            acted(4000, "a", "Read"),
            acted(7000, "a", "Edit"),
            acted(10_000, "a", "Edit"),
        ]);
        let level = level(&a);
        let on_a_shelf = |t| {
            let s = stage(&a, t);
            let actor = &s.actors[0];
            (actor.x, actor.bottom < level.ground - 1)
        };
        let (first, second) = (on_a_shelf(3500), on_a_shelf(6500));
        assert!(first.1 && second.1, "up on the shelves");
        assert_ne!(first.0, second.0, "another shelf");
        let blocks = |t| stage(&a, t).blocks;
        let before = blocks(6500);
        assert_eq!(before.len(), blocks(9500).len(), "the same blocks");
        assert_ne!(before, blocks(9500), "one moved");
        assert_ne!(blocks(9500), blocks(20_000), "and another");
        assert_eq!(blocks(20_000), blocks(900_000), "where they were left");
    }

    #[test]
    fn a_kill_flashes_the_edge_and_cracks_the_ground_deeper_each_time() {
        let a = mori(&[
            spawn(0, "a", "m"),
            acted(1000, "a", "Bash"),
            killed(5000, "a", "pattern", 1, false, "git push -f"),
            killed(10_000, "a", "pattern", 2, false, "git push -f"),
        ]);
        let at = |t| stage(&a, t);
        let start = (5000..6000).step_by(25).find(|&t| !at(t).effects.is_empty()).expect("the edge flashes");
        assert!(at(start).effects.iter().all(|e| matches!(e, Effect::Flash { .. })), "{:?}", at(start).effects);
        assert!(at(4900).cracks.is_empty());
        let once = at(9000).cracks.len();
        assert!(once > 0, "the ground cracked");
        assert!(at(14_000).cracks.len() > once, "deeper the second time");
        let line = at(start).narration.expect("a kill is always narrated");
        assert!(line.text.contains("run git push -f") || line.text.contains("bounced"), "{}", line.text);
        assert_eq!(at(20_000).actors[0].x, level(&a).x(Spot::Landing, 0, 3), "back where it was");
    }

    fn method(a: &Mori) -> Method {
        match a.actors[0].beats.back().map(|b| b.act) {
            Some(Act::Die(m)) => m,
            other => panic!("not dead: {other:?}"),
        }
    }

    /// One character dying alone; which death it gets is up to the hash of its id.
    fn alone(id: &str, rule: &str) -> Mori {
        mori(&[spawn(0, id, "m"), killed(1000, id, "pattern", 3, true, "x"), died(1001, id, rule, "strikes")])
    }

    /// A leader and three helpers who all go at once; which way is up to the hash of the leader.
    fn together(id: &str) -> Mori {
        let mut log = vec![spawn(0, id, "m")];
        for (i, at) in [(1, 1000), (2, 1300), (3, 1600)] {
            let helper = format!("{id}-{i}");
            log.extend([sub(10 * i, &helper, id), killed(at, &helper, "pattern", 3, true, "x")]);
            log.push(died(at + 1, &helper, "r", "strikes"));
        }
        mori(&log)
    }

    #[test]
    fn deaths_are_picked_at_random_and_each_has_its_scene_and_epitaph() {
        assert_eq!(method(&alone("a", super::super::SELF_RULE)), Method::Scythe, "looking for reaper: reaper comes");
        let mut seen = Vec::new();
        for i in 0..400 {
            let id = format!("agent-{i}");
            let a = alone(&id, "r");
            let m = method(&a);
            if seen.contains(&m) {
                continue;
            }
            seen.push(m);
            let start = a.actors[0].death.unwrap().at;
            let stood = stage(&a, start - 1).actors[0].clone();
            let after = stage(&a, start + 6000);
            let actor = &after.actors[0];
            assert_eq!(actor.label, Some(Label::Epitaph(m.epitaph())), "{m:?}");
            assert!(actor.grave && actor.fill == DEAD && !actor.alive, "{m:?}");
            assert_eq!((actor.w, actor.h), CROSS, "{m:?}: a little cross");
            assert!(after.effects.is_empty(), "{m:?} cleaned up: {:?}", after.effects);
            let bloody = !after.stains.is_empty();
            assert_eq!(bloody, !matches!(m, Method::Enlightened), "{m:?} and blood");
            assert!(after.stains.iter().all(|s| s.2 != BLOOD), "{m:?}: dried once it's over");
            let during: Vec<Stage> = (start..start + 3500).step_by(100).map(|t| stage(&a, t)).collect();
            assert!(during.iter().any(|s| !s.effects.is_empty() || s.actors[0].h != stood.h), "{m:?} plays a scene");
            if m == Method::Floor {
                assert!(actor.bottom > stood.bottom, "down in the hole: {actor:?} {stood:?}");
                assert_ne!(after.land, level(&a).top, "the floor gave way");
            }
            let line = stage(&a, start + 100).narration.expect("deaths are narrated").text;
            assert!(line.contains(actor.name), "{m:?}: {line}");
        }
        seen.sort_by_key(|m| m.epitaph());
        let mut all = super::super::ALONE.to_vec();
        all.sort_by_key(|m| m.epitaph());
        assert_eq!(seen, all, "every death alone turns up");
    }

    #[test]
    fn the_holy_hand_grenade_is_held_up_and_counted_to_five() {
        let a = (0..400)
            .map(|i| alone(&format!("agent-{i}"), "r"))
            .find(|a| method(a) == Method::Grenade)
            .expect("someone gets the grenade");
        let start = a.actors[0].death.unwrap().at;
        let orb = |s: &Stage| {
            s.effects.iter().find_map(|e| match e {
                Effect::Sprite { sprite, bottom, .. } if std::ptr::eq(*sprite, &GRENADE) => Some(*bottom),
                _ => None,
            })
        };
        let frames: Vec<Stage> = (start..start + 6000).step_by(50).map(|t| stage(&a, t)).collect();
        let mut said = Vec::new();
        for s in &frames {
            let me = &s.actors[0];
            if let Some(Label::Says(word)) = me.label
                && said.last() != Some(&word)
            {
                said.push(word);
            }
            if let Some(bottom) = orb(s) {
                assert!(bottom < me.bottom - me.h, "over its head, clear of it: {bottom} {me:?}");
            }
        }
        assert!(orb(&frames[0]).is_some(), "it's there from the start");
        assert_eq!(said, ["one!", "two!", "five!"], "as instructed, more or less");
        let boom = frames.iter().find(|s| orb(s).is_none() && s.actors[0].hidden).expect("it goes off");
        assert_eq!(boom.actors[0].label, Some(Label::Says("five!")), "the last number still hanging there");
    }

    #[test]
    fn several_at_once_go_one_after_another_the_same_way() {
        let mut seen = Vec::new();
        for i in 0..200 {
            let a = together(&format!("lead-{i}"));
            let helpers: Vec<Method> = (1..4)
                .map(|k| match a.actors[k].beats.back().map(|b| b.act) {
                    Some(Act::Die(m)) => m,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert!(helpers.iter().all(|&m| m == helpers[0]), "the same way: {helpers:?}");
            let m = helpers[0];
            assert!(super::super::GROUP.contains(&m), "{m:?}");
            let starts: Vec<i64> = (1..4).map(|k| a.actors[k].death.unwrap().at).collect();
            assert!(starts.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] <= 2000), "one after another: {starts:?}");
            if seen.contains(&m) {
                continue;
            }
            seen.push(m);
            let after = stage(&a, starts[2] + 4000);
            for (k, actor) in after.actors[1..].iter().enumerate() {
                let epitaph = (k == 0).then_some(Label::Epitaph(m.epitaph()));
                assert_eq!(actor.label, epitaph, "{m:?}: one epitaph, on the first to go");
                assert!(!actor.alive && actor.fill == DEAD);
            }
            let crosses = after.actors[1..].iter().filter(|a| a.grave && !a.hidden).count();
            if m == Method::Cliff {
                let (g0, g1) = level(&a).gap();
                let first = &after.actors[1];
                assert!(first.x >= g0 - 1 && first.x + first.w <= g1 + 1, "in the gap: {first:?}");
                assert_eq!(crosses, 1, "one cross for the heap");
                assert_eq!(after.land, level(&a).top, "they jumped: the floor held");
            } else {
                assert!(crosses >= 1, "{m:?}");
            }
        }
        assert_eq!(seen.len(), super::super::GROUP.len(), "every way of going together turns up: {seen:?}");
    }

    #[test]
    fn a_session_never_sees_the_same_death_twice_running() {
        let mut log = vec![spawn(0, "lead", "m")];
        for i in 0..12 {
            let id = format!("h{i}");
            let at = 1000 + i * 10_000;
            log.extend([
                sub(at - 500, &id, "lead"),
                killed(at, &id, "pattern", 3, true, "x"),
                died(at + 1, &id, "r", "strikes"),
            ]);
        }
        let a = mori(&log);
        let deaths: Vec<Method> = a.actors[1..]
            .iter()
            .map(|c| match c.beats.back().map(|b| b.act) {
                Some(Act::Die(m)) => m,
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(deaths.windows(2).all(|d| d[0] != d[1]), "{deaths:?}");
        let again = mori(&log);
        assert_eq!(
            format!("{:?}", a.scene(80_000, 80, 22)),
            format!("{:?}", again.scene(80_000, 80, 22)),
            "the same every replay"
        );
    }

    #[test]
    fn subagents_climb_out_of_their_parent_in_their_own_colour() {
        let a = mori(&[spawn(0, "main", "claude-opus-5"), sub(1000, "s1", "main"), sub(3000, "s2", "main")]);
        let s = stage(&a, 9000);
        let (main, s1, s2) = (&s.actors[0], &s.actors[1], &s.actors[2]);
        assert!(main.hue != s1.hue && s1.hue != s2.hue && main.hue != s2.hue);
        assert_eq!((s1.w, s1.h), (2, 3), "half of an opus");
        assert_eq!(s.exits.len(), 3, "an exit each");
        assert_eq!(s.exits[1].w, 2, "the shape of its own outline");
        let during = stage(&a, 1150);
        let (parent, child) = (&during.actors[0], &during.actors[1]);
        assert!(parent.h > 6, "the parent stretches: {parent:?}");
        assert_eq!(child.bottom, parent.bottom - parent.h, "on top of its parent: {child:?} {parent:?}");
        assert_eq!(stage(&a, 9000).actors[0].h, 6, "and is itself again");
        assert_eq!(s.title, None);
    }

    #[test]
    fn at_the_end_of_its_turn_it_walks_into_its_exit_and_comes_back_out_to_work() {
        let a = mori(&[
            spawn(0, "a", "claude-opus-5"),
            acted(1000, "a", "Read"),
            event(5000, "a", "agent.idle", ""),
            acted(20_000, "a", "Edit"),
        ]);
        let s = stage(&a, 15_000);
        let (actor, exit) = (&s.actors[0], s.exits[0]);
        assert!(actor.home && actor.x == exit.x && actor.bottom == exit.bottom, "{actor:?} {exit:?}");
        assert!(!stage(&a, 4900).actors[0].home);
        let out = stage(&a, 25_000);
        assert!(!out.actors[0].home && out.actors[0].x < exit.x, "back out: {:?}", out.actors[0]);
        assert_eq!((out.exits[0].x, out.exits[0].bottom), (exit.x, exit.bottom), "the exit stays, waiting");
        assert!(exit.lit && !out.exits[0].lit, "lit only while it's home");
    }

    #[test]
    fn other_sessions_are_strips_and_their_deaths_get_a_line() {
        let a = mori(&[
            spawn(0, "a", "m"),
            spawn(10, "b", "m"),
            killed(100, "b", "watcher", 1, true, "x"),
            died(101, "b", "r", "strikes"),
            acted(30_000, "a", "Read"),
        ]);
        // Nobody chose a session: the stage follows the action, but stays on b's death a while.
        assert_eq!(a.followed(), Some(0), "a was busy again long after b's death");
        let s = a.scene(30_100, 80, 22);
        assert_eq!(s.strips.len(), 2);
        assert!(s.strips[0].followed && !s.strips[1].followed);
        assert!(!s.strips[1].minis[0].alive);
        let early = mori(&[
            spawn(0, "a", "m"),
            spawn(10, "b", "m"),
            killed(100, "b", "watcher", 1, true, "x"),
            died(101, "b", "r", "strikes"),
        ]);
        assert_eq!(early.followed(), Some(0), "a quiet session keeps the stage for a while");
        let kill = early.sessions[1].kill_at.unwrap();
        assert!(early.scene(kill + 100, 80, 22).strips[1].flash, "a kill flashes the strip");
        let death = early.actors[1].death.unwrap().at;
        let s = early.scene(death + 100, 80, 22);
        assert!(!s.strips[1].flash);
        assert!(s.strips[1].line.is_some(), "a death gets a line beside the strip");
    }

    #[test]
    fn a_session_where_everyone_died_leaves_a_minute_later() {
        let a = mori(&[
            spawn(0, "a", "m"),
            spawn(10, "b", "m"),
            killed(100, "b", "pattern", 3, true, "x"),
            died(101, "b", "r", "strikes"),
        ]);
        let death = a.actors[1].death.unwrap().at;
        let numbers = |t| a.scene(t, 80, 22).strips.iter().map(|s| s.number).collect::<Vec<_>>();
        assert_eq!(numbers(death + 30_000), [1, 2], "the grave stays a while");
        assert_eq!(numbers(death + GONE_TEST), [1], "then the session is cleared away");
        assert_eq!(a.scene(death + GONE_TEST - 1, 80, 22).next, Some(death + GONE_TEST), "redrawn when it goes");
        let mut chose = a.clone();
        chose.choose(1);
        assert_eq!(chose.scene(death + GONE_TEST, 80, 22).stage.unwrap().number, 1, "the stage moves on");
        chose.switch(1, death + GONE_TEST);
        assert_eq!(chose.followed(), Some(0), "and it can't be switched to");

        let mut back =
            mori(&[spawn(0, "b", "m"), killed(100, "b", "pattern", 3, true, "x"), died(101, "b", "r", "strikes")]);
        assert_eq!(back.scene(100_000, 80, 22).strips.len(), 0, "gone");
        back.push(parse_line(&acted(200_000, "b", "Read")));
        back.advance(200_000);
        assert_eq!(back.scene(201_000, 80, 22).strips.len(), 1, "a new prompt brings it back");
    }

    #[test]
    fn a_closed_session_goes_home_and_leaves() {
        let numbers = |m: &Mori, t| m.scene(t, 80, 22).strips.iter().map(|s| s.number).collect::<Vec<_>>();
        let mut a = mori(&[
            spawn(0, "a", "m"),
            spawn(10, "b", "m"),
            sub(20, "b1", "b"),
            acted(100, "b1", "Read"),
            event(20_000, "b", "agent.ended", ""),
        ]);
        assert!(a.actors[1].home && a.actors[2].home, "everyone goes home, the subagent too");
        assert_eq!(a.followed(), Some(0), "leaving doesn't take the stage");
        let bye = a.sessions[1].moments.back().filter(|m| m.section == Section::Closed).expect("a goodbye");
        assert_eq!(a.actors[1].ended, Some(bye.at + life(&bye.text)), "nobody leaves before it's read");
        let home = a.actors[1].ended.unwrap().max(a.actors[2].ended.unwrap());
        assert_eq!(numbers(&a, home), [1, 2]);
        assert_eq!(a.scene(home, 80, 22).next, Some(home + DEPART_TEST), "redrawn when it goes");
        assert_eq!(numbers(&a, home + DEPART_TEST), [1], "then the session leaves");
        a.push(parse_line(&spawn(40_000, "b", "m")));
        a.advance(40_000);
        assert_eq!(numbers(&a, 41_000), [1, 2], "resuming it brings it back");

        // Reaping a Claude Code session closes it too, just before the reap is recorded.
        let reaped = mori(&[
            spawn(0, "a", "m"),
            event(100, "a", "agent.idle", ""),
            event(1_000, "a", "agent.ended", ""),
            event(1_500, "a", "agent.reaped", r#""idle":1400"#),
        ]);
        assert!(reaped.actors[0].asleep);
        assert_eq!(numbers(&reaped, 100_000), [1], "the reaped stay, to be resumed");
        assert!(reaped.sessions[0].moments.iter().all(|m| m.section != Section::Closed), "and say no goodbye");
    }

    #[test]
    fn a_new_session_takes_the_lowest_number_free_and_strips_go_in_number_order() {
        let numbers = |m: &Mori, t| m.scene(t, 80, 22).strips.iter().map(|s| s.number).collect::<Vec<_>>();
        let mut a =
            mori(&[spawn(0, "a", "m"), spawn(10, "b", "m"), spawn(20, "c", "m"), event(1_000, "b", "agent.ended", "")]);
        let gone = a.actors[1].ended.unwrap() + DEPART_TEST;
        assert_eq!(numbers(&a, gone), [1, 3], "the others keep theirs");
        a.push(parse_line(&spawn(gone, "d", "m")));
        a.push(parse_line(&spawn(gone + 10, "e", "m")));
        a.advance(gone + 10);
        assert_eq!(numbers(&a, gone + 20), [1, 2, 3, 4], "2 is taken again, then the next free");
        a.choose(0);
        a.switch(1, gone + 30);
        assert_eq!(a.followed(), Some(3), "switching goes along the row");

        // Back after leaving: its old number if that's still free, else the lowest that is.
        a.push(parse_line(&spawn(gone + 40, "b", "m")));
        a.advance(gone + 40);
        assert_eq!(a.sessions[1].number, 5, "2 went to d meanwhile");
    }

    #[test]
    fn the_stage_has_what_the_human_first_asked_for() {
        let a = mori(&[
            spawn(0, "a", "m"),
            event(10, "a", "agent.prompted", r#""prompt":"fix the tests""#),
            sub(20, "a1", "a"),
            event(30, "a", "agent.prompted", r#""prompt":"and the docs""#),
        ]);
        assert_eq!(stage(&a, 5_000).prompt.as_deref(), Some("fix the tests"));
        assert_eq!(a.actors.len(), 2, "a prompt casts nobody");
    }

    #[test]
    fn a_session_quiet_for_half_a_day_leaves_until_it_does_something() {
        let quiet = crate::state::QUIET_FOR;
        let numbers = |m: &Mori, t| m.scene(t, 80, 22).strips.iter().map(|s| s.number).collect::<Vec<_>>();
        let mut a = mori(&[spawn(0, "a", "m"), event(100, "a", "agent.idle", "")]);
        assert_eq!(numbers(&a, 99 + quiet), [1]);
        assert!(numbers(&a, 100 + quiet).is_empty(), "most likely ended unheard");
        a.push(parse_line(&acted(200 + quiet, "a", "Read")));
        a.advance(200 + quiet);
        assert_eq!(numbers(&a, 300 + quiet), [1], "back as soon as it does something");
    }

    #[test]
    fn a_helper_that_hands_its_work_back_is_done_and_one_never_seen_working_was_never_a_session() {
        let a = mori(&[
            spawn(0, "a", "m"),
            sub(1000, "s1", "a"),
            acted(5000, "s1", "SubagentHandback"),
            event(5100, "s1", "agent.idle", ""),
            event(9000, "ghost", "agent.idle", ""),
        ]);
        let done = a.actors[1].ended.expect("handing back is its last act");
        let goodbyes = a.sessions[0].moments.iter().filter(|m| m.section == Section::Done).count();
        assert_eq!(goodbyes, 1, "said once, though it also said it stopped");
        assert_eq!(stage(&a, done + DEPART_TEST).actors.len(), 1);
        assert!(a.sessions[1].moments.is_empty(), "the ghost says nothing");
        let gone = a.actors[2].ended.unwrap() + DEPART_TEST;
        let numbers = a.scene(gone, 80, 22).strips.iter().map(|s| s.number).collect::<Vec<_>>();
        assert_eq!(numbers, [1], "and is gone");
    }

    #[test]
    fn a_subagent_whose_work_is_done_goes_home_and_leaves_for_good() {
        let mut a = mori(&[
            spawn(0, "a", "m"),
            sub(1000, "s1", "a"),
            sub(1100, "s2", "a"),
            acted(2000, "s1", "Read"),
            event(5000, "s1", "agent.idle", ""),
        ]);
        let left = a.actors[1].ended.expect("its work is done");
        assert!(a.actors[1].home && a.actors[2].ended.is_none() && a.actors[0].ended.is_none());
        let word = a.sessions[0].moments.back().filter(|m| m.section == Section::Done).expect("a word on the way out");
        assert_eq!(left, word.at + life(&word.text), "and it stays until that's read");
        let midway = (word.at + left) / 2;
        assert!(stage(&a, midway).narration.is_some_and(|n| n.text == word.text), "said above it");
        let on = |a: &Mori, t| stage(a, t).actors.len();
        assert_eq!(on(&a, left), 3, "home first");
        assert_eq!(on(&a, left + DEPART_TEST), 2, "then gone");
        assert_eq!(a.scene(left + DEPART_TEST, 80, 22).strips[0].minis.len(), 2, "from the strip too");
        a.switch(1, left + 10_000);
        let title = stage(&a, left + 10_500).title.unwrap().text;
        assert!(title.ends_with("and company"), "one helper still there: {title}");
        a.pick(1, left + 11_000);
        a.pick(1, left + 12_000);
        assert_eq!(a.picked(), Some("s2"), "the one who left can't be picked");
    }

    #[test]
    fn a_death_can_be_played_for_fun_and_clears_itself_away() {
        let mut a = mori(&[spawn(0, "a", "m"), acted(500, "a", "Read")]);
        for (k, expected) in [Method::Scythe, Method::Blade, Method::Floor].into_iter().enumerate() {
            let now = 10_000 + k as i64 * 30_000;
            assert_eq!(a.rehearse(now), expected, "each in turn");
            let s = a.scene(now + 500, 80, 22);
            assert_eq!(s.strips.len(), 1, "never a strip");
            let stage = s.stage.unwrap();
            assert_eq!(stage.title.unwrap().text, format!("rehearsal · {}", expected.epitaph()));
            let after = a.scene(now + 6000, 80, 22).stage.unwrap();
            assert!(after.actors.iter().all(|c| c.label == Some(Label::Epitaph(expected.epitaph()))), "{expected:?}");
            let line = a.scene(now + 6000, 80, 22).stage.unwrap().narration.expect("the death is told");
            assert_eq!(line.shown, line.text.chars().count(), "typed out");
            let start = a.actors.last().unwrap().death.unwrap().at;
            assert!(start - now >= 2000, "a beat before it starts");
            assert!(
                start + super::life(&line.text) <= now + 2000 + super::super::REHEARSAL_GONE,
                "read before it goes"
            );
            assert_eq!(a.scene(now + 20_000, 80, 22).stage.unwrap().number, 1, "back to the real session");
        }
        // A group death brings a small cast.
        let mut b = mori(&[spawn(0, "a", "m")]);
        for _ in 0..6 {
            b.rehearse(1000);
        }
        let now = 100_000;
        assert_eq!(b.rehearse(now), Method::Cliff);
        let stage = b.scene(now + 1000, 80, 22).stage.unwrap();
        assert_eq!(stage.actors.len(), 3);
        b.choose(0);
        let real = mori(&[spawn(0, "a", "m"), spawn(10, "z", "m")]);
        assert_eq!(real.scene(100, 80, 22).strips[1].number, 2);
        assert_eq!(b.sessions.last().unwrap().number, 0, "rehearsals take no number");
    }

    #[test]
    fn switching_shows_a_title_card_and_picking_answers_with_a_line() {
        let mut a = mori(&[spawn(0, "a", "m"), spawn(10, "b", "m"), sub(20, "b1", "b")]);
        a.switch(1, 10_000);
        assert_eq!(a.followed(), Some(1));
        let s = a.scene(10_500, SIZE.0, SIZE.1).stage.unwrap();
        let lead = a.name("b").unwrap();
        assert_eq!(s.title.unwrap().text, format!("2 · {lead} and company"));
        let team = a.scene(12_000, SIZE.0, SIZE.1).stage.unwrap().narration.expect("a line about the team");
        assert!(team.text.contains(lead) || team.text.starts_with("There were two"), "{}", team.text);

        a.pick(1, 20_000);
        assert_eq!(a.picked(), Some("b"));
        let s = a.scene(20_100, SIZE.0, SIZE.1).stage.unwrap();
        assert!(s.actors[0].picked && s.actors[0].label == Some(Label::Name { faint: false }));
        let answer = s.narration.expect("picking answers at once");
        assert_eq!(answer.row, s.actors[0].voice.1);
        a.pick(1, 21_000);
        assert_eq!(a.picked(), Some("b1"));
        a.pick(1, 22_000);
        assert_eq!(a.picked(), Some("b1"), "stops at the end");
    }

    #[test]
    fn those_at_home_get_lines_about_home_not_work() {
        let mut a = mori(&[
            spawn(0, "a", "m"),
            acted(100, "a", "Read"),
            event(1_000, "a", "agent.idle", ""),
            spawn(2_000, "b", "m"),
            acted(2_100, "b", "Read"),
        ]);
        let home = |m: &Mori, agent: &str, text: &str| {
            let name = m.name(agent).unwrap();
            super::super::lines::BANK.iter().any(|l| l.when == When::Home && l.text.replace("{name}", name) == text)
        };
        a.choose(1);
        a.switch(1, 10_000);
        assert_eq!(a.followed(), Some(0));
        let line = stage(&a, 12_000).narration.expect("a line after the title card");
        assert!(home(&a, "a", &line.text), "switched to: {}", line.text);
        a.pick(1, 20_000);
        assert!(home(&a, "a", &stage(&a, 20_100).narration.unwrap().text), "picked at home");
        a.switch(1, 30_000);
        a.pick(1, 40_000);
        let line = stage(&a, 40_100).narration.unwrap();
        assert!(!home(&a, "b", &line.text) && !line.text.contains("home"), "picked at work: {}", line.text);
    }

    #[test]
    fn a_death_line_stays_up_until_something_newer_is_said() {
        let mut a = mori(&[
            spawn(0, "a", "m"),
            sub(10, "b", "a"),
            killed(1000, "b", "pattern", 3, true, "x"),
            died(1001, "b", "r", "strikes"),
        ]);
        let start = a.actors[1].death.unwrap().at;
        let line = |a: &Mori, t| stage(a, t).narration;
        let death = line(&a, start + 100).expect("told");
        let later = line(&a, start + 40_000).expect("still up, long after it would have faded");
        assert_eq!((later.text.as_str(), later.faint), (death.text.as_str(), 0));
        assert_ne!(a.scene(start + 40_000, 80, 22).next, Some(start + 40_000 + FRAME), "and still: no frames for it");
        a.pick(1, start + 50_000);
        let answer = line(&a, start + 50_100).expect("the answer");
        assert_ne!(answer.text, death.text);
        let after = line(&a, start + 70_000);
        assert!(after.as_ref().is_none_or(|n| n.text != death.text), "it doesn't come back: {after:?}");
    }

    #[test]
    fn the_most_important_line_wins_and_nothing_queues() {
        let a = mori(&[
            spawn(0, "a", "m"),
            killed(5000, "a", "pattern", 1, false, "x"),
            acted(5500, "a", "Read"),
            acted(5600, "a", "Read"),
            acted(5700, "a", "Read"),
            acted(5800, "a", "Read"),
            acted(5900, "a", "Read"),
        ]);
        let killed_line = a.sessions[0].moments.iter().find(|m| m.section == Section::Killed).unwrap();
        let line = stage(&a, killed_line.at + 1000).narration.unwrap();
        assert_eq!(line.text, killed_line.text, "the kill outranks working");
    }

    #[test]
    fn nothing_moves_unless_it_is_doing_something() {
        // At the window, then: another web search there, a tool with no place, a redirect.
        // None of it is going anywhere. Then its turn ends and, once home, it stays put.
        let a = mori(&[
            spawn(0, "a", "m"),
            acted(1000, "a", "WebFetch"),
            acted(4000, "a", "WebSearch"),
            acted(5000, "a", "TodoWrite"),
            event(6000, "a", "agent.redirected", r#""rule":"r""#),
            event(30_000, "a", "agent.idle", ""),
        ]);
        let body = |t| {
            let s = stage(&a, t);
            let actor = &s.actors[0];
            (actor.x, actor.bottom, actor.w, actor.h, actor.fill, actor.home)
        };
        let rest = body(3900);
        for t in (3900..30_000).step_by(25) {
            assert_eq!(body(t), rest, "moved at {t}");
        }
        let home = body(40_000);
        assert!(home.5);
        for t in (40_000..300_000).step_by(100) {
            assert_eq!(body(t), home, "moved at {t}");
        }
    }

    #[test]
    fn a_reaped_character_lies_down_in_its_own_colour_and_gets_up_when_resumed() {
        let a = mori(&[
            spawn(0, "a", "claude-opus-5"),
            event(1000, "a", "agent.idle", ""),
            event(400_000, "a", "agent.reaped", r#""idle":300000"#),
            spawn(900_000, "a", "claude-opus-5"),
        ]);
        let actor = |t| stage(&a, t).actors[0].clone();
        let (up, hue) = (actor(300_000), actor(300_000).hue);
        assert!(up.home, "it went home first");
        let asleep = actor(402_000);
        assert_eq!((asleep.w, asleep.h), (6, 3), "lying flat");
        assert!(asleep.alive && !asleep.outline && !asleep.home, "asleep, not dead");
        assert!(asleep.fill != DEAD && asleep.fill != hue, "its own colour, dimmed");
        assert_eq!(asleep.x + asleep.w / 2, up.x + up.w / 2, "where it stood");
        let line = stage(&a, 401_500).narration.expect("lying down is narrated").text;
        assert!(line.contains(asleep.name), "{line}");

        let mut minis = a.scene(402_000, SIZE.0, SIZE.1).strips[0].minis[0];
        assert!(minis.alive && minis.asleep);
        let awake = actor(902_000);
        assert_eq!((awake.w, awake.h, awake.fill, awake.home), (up.w, up.h, hue, true), "standing again, at home");
        assert!(stage(&a, 900_800).narration.is_some_and(|n| n.text.contains(awake.name)));
        minis = a.scene(902_000, SIZE.0, SIZE.1).strips[0].minis[0];
        assert!(!minis.asleep);
    }

    #[test]
    fn a_kill_takes_a_piece_of_its_health_and_nothing_gives_it_back() {
        let kill = |at: i64, strike: u32, pressure: f64, last: bool| {
            let fields = format!(
                r#""tool":"bash","rule":"r","tier":"pattern","strike":{strike},"final":{last},"attempt":"","pressure":{pressure}"#
            );
            event(at, "a", "agent.killed", &fields)
        };
        let mut log = vec![spawn(0, "a", "claude-opus-5"), kill(1000, 1, 0.5, false)];
        log.extend((0..10).map(|i| acted(10_000 + 3000 * i, "a", "Edit")));
        log.extend([
            event(45_000, "a", "agent.idle", ""),
            kill(50_000, 2, 1.0, true),
            died(50_001, "a", "r", "strikes"),
        ]);
        let a = mori(&log);
        let health = |t| stage(&a, t).health[0];
        assert_eq!(health(500).left, 1.0, "untouched");
        assert_eq!(health(1100).left, 1.0, "not until it hits the edge");
        assert!((health(5000).left - 0.5).abs() < 1e-3, "{:?}", health(5000));
        assert!((health(42_000).left - 0.5).abs() < 1e-3, "ten edits give none of it back: {:?}", health(42_000));
        assert!(!health(42_000).dead);
        assert!(health(80_000).dead, "flat once it's dead");

        let mini = |t| a.scene(t, SIZE.0, SIZE.1).strips[0].minis[0];
        assert!(!mini(42_000).home, "at work");
        assert!(mini(46_000).home, "waiting for its next turn");
    }

    #[test]
    fn small_moris_give_up_names_and_never_break() {
        let a = mori(&[spawn(0, "a", "m")]);
        assert!(Level::new(&a.sessions[0].plan, 100, 28).names);
        assert!(!Level::new(&a.sessions[0].plan, 40, 10).names);
        for (cols, rows) in [(1, 1), (5, 3), (20, 5), (300, 90)] {
            for tier in ["pattern", "watcher"] {
                let a = mori(&[
                    spawn(0, "a", "m"),
                    sub(10, "b", "a"),
                    acted(15, "a", "Edit"),
                    event(16, "b", "agent.idle", ""),
                    killed(20, "a", tier, 1, false, "x"),
                    killed(30, "a", tier, 3, true, "x"),
                    died(31, "a", "r", "strikes"),
                ]);
                for t in [0, 300, 1000, 2000, 5000, 9000] {
                    a.scene(t, cols, rows);
                }
            }
        }
    }
}
