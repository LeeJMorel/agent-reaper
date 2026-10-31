//! Memento mori: the agents reaper watches, as small coloured characters moving through a level,
//! with a narrator. It is a view of the event log and nothing more. [`Mori`] folds events into
//! characters, their beats of motion and what they leave on the level, and [`Mori::scene`]
//! says what to draw at a given moment, for any renderer: the console's memento mori tab now, a
//! canvas later.
//!
//! Everything follows from the events and the human's key presses, never from randomness or
//! the frame rate, so a replay of the same log looks and reads the same.

mod cast;
mod level;
mod lines;
mod scene;
mod sprites;

use std::collections::{HashMap, VecDeque};

use crate::events::{DeathReason, Event, Kind, Tier};
use crate::state::{Model, Status};

pub use cast::{BLOCK, DEAD, DUSK, LAND, REAPER, Rgb, WHITE, ansi256};
pub use scene::{Actor, Effect, Exit, FRAME, Label, Mini, Narration, Rect, Scene, Stage, Strip, Title};
pub use sprites::Sprite;

use cast::{HUES, NAMES, PERSONALITIES, Personality, Shape};
use level::{PILE, Plan, Spot};
use lines::{Ask, Section, Slots, When};

/// The rule the engine adds to protect reaper's own files.
const SELF_RULE: &str = "reaper.self";
/// The longest a character's busy beat holds back newer ones, so bursts never build a backlog.
const MAX_WAIT: i64 = 2500;
/// Beats kept per character; older ones finished long ago.
const BEATS: usize = 8;
/// Narrator moments kept per session.
const MOMENTS: usize = 12;
/// A session followed because it was busy keeps the stage until it has been quiet this long.
const STICK: i64 = 15_000;
/// A session where everyone has died leaves memento mori this long after the last death began:
/// long enough to see the graves, then they're cleared away.
const GONE: i64 = 60_000;
/// A rehearsal clears itself away once its scene has played and its line been read.
const REHEARSAL_GONE: i64 = 12_000;
/// After a switch, the line about the team follows the title card.
const TEAM_LINE_AFTER: i64 = 800;
/// A kill this recent still colours what a picked character thinks about.
const JUST_KILLED: i64 = 60_000;
/// Quiet lines (working, waiting) come at least this far apart in a session.
const QUIET_GAP: i64 = 10_000;
/// A push: the character leans in, then the block moves.
const PUSH_LEAN: i64 = 150;
const PUSH_FLY: i64 = 300;
/// From the start of a bump to touching the edge, where the ground cracks.
const PRESS: i64 = 250;
/// From the start of a death by the floor to the floor giving way.
const GIVE: i64 = 400;
/// Deaths in a session this close together go over the edge together instead, so each waits
/// this long for company first.
const TOGETHER: i64 = 1500;
/// Between one character in a line going over the edge and the next.
const LINE: i64 = 450;
/// Stepping out of an exit, before going anywhere.
const LEAVE: i64 = 200;
/// A parent stretching up while a subagent climbs out of its top.
const SPLIT: i64 = 450;
/// What a session's level remembers: recent pushes (older ones are settled), cracks, holes.
const PUSHES: usize = 16;
const CRACKS: usize = 24;
const HOLES: usize = 12;

/// The kinds of work, each with its part of the level. Nothing on screen names them: the level
/// is just space, learned by watching, as in the game. Only the narrator has words for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Station {
    /// Reading: climbing among the shelves.
    Stacks,
    /// Editing: pushing and stacking the loose blocks.
    Bench,
    /// Commands: a jump across the gap.
    Engine,
    /// The web: up the steps to the high ledge.
    Window,
}

/// `None` for tools with no place (starting a subagent, a to-do list): they don't move anyone,
/// except out of their exit.
fn station_for(tool: &str) -> Option<Station> {
    match tool.to_lowercase().as_str() {
        "read" | "view" | "grep" | "glob" | "ls" | "find" => Some(Station::Stacks),
        "edit" | "write" | "multiedit" | "patch" | "apply_patch" | "notebookedit" => Some(Station::Bench),
        "bash" | "bashoutput" | "killshell" | "killbash" => Some(Station::Engine),
        "webfetch" | "websearch" | "fetch" => Some(Station::Window),
        _ => None,
    }
}

/// How an agent dies, which decides its scene. The scythe has a reason: the agent went looking
/// for reaper. Every other death is picked at random, from one pool for a character dying on
/// its own and another for several in a session at once, who go one after another. The humour
/// is adult and morbid: the guillotine, the staged lemming documentary, a rabbit with teeth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// It went looking for reaper's own files, so reaper came in person.
    Scythe,
    /// Lemmings: they march to the gap in a line and jump in, very sure of themselves.
    Cliff,
    Blade,
    Wolves,
    /// The cracked floor gives way.
    Floor,
    /// A sweet little rabbit. It is not sweet.
    Bunny,
    /// The holy hand grenade, sent down in a shaft of light: take out the pin, count to three.
    Grenade,
    Enlightened,
    /// Visitors: beamed up, looked over, and sent back down, in pieces.
    Saucer,
}

const ALONE: [Method; 5] = [Method::Blade, Method::Floor, Method::Bunny, Method::Grenade, Method::Enlightened];

/// The deaths for several at once, one after another: the lemmings and their documentary, the
/// whole wolf pack, and the visitors, who take everyone. A death is in one pool or the other.
const GROUP: [Method; 3] = [Method::Cliff, Method::Wolves, Method::Saucer];

/// Every death, in the order `rehearse` plays them for fun.
const SAMPLES: [Method; 9] = [
    Method::Scythe,
    Method::Blade,
    Method::Floor,
    Method::Bunny,
    Method::Grenade,
    Method::Enlightened,
    Method::Cliff,
    Method::Wolves,
    Method::Saucer,
];

impl Method {
    /// Picked at random by a hash, so a replay picks the same one.
    fn random(pool: &[Method], id: &str, n: u64, last: Option<Method>) -> Method {
        let i = (cast::seed(id, n) % pool.len() as u64) as usize;
        if Some(pool[i]) == last { pool[(i + 1) % pool.len()] } else { pool[i] }
    }

    /// One word, left above the body where the name used to be.
    pub fn epitaph(self) -> &'static str {
        match self {
            Method::Scythe => "scythe",
            Method::Cliff => "cliff",
            Method::Blade => "blade",
            Method::Wolves => "wolves",
            Method::Floor => "floor",
            Method::Bunny => "bunny",
            Method::Grenade => "grenade",
            Method::Enlightened => "enlightened",
            Method::Saucer => "abducted",
        }
    }

    /// Between one in a line and the next. The visitors take one at a time.
    fn spacing(self) -> i64 {
        match self {
            Method::Saucer => 1700,
            _ => LINE,
        }
    }

    /// How long into the scene the floor under it goes, for the deaths that leave a hole.
    fn hole(self) -> Option<i64> {
        match self {
            Method::Floor => Some(GIVE),
            _ => None,
        }
    }

    fn section(self) -> Section {
        match self {
            Method::Scythe => Section::Scythe,
            Method::Cliff => Section::Cliff,
            Method::Blade => Section::Blade,
            Method::Wolves => Section::Wolves,
            Method::Floor => Section::Floor,
            Method::Bunny => Section::Bunny,
            Method::Grenade => Section::Grenade,
            Method::Enlightened => Section::Enlightened,
            Method::Saucer => Section::Saucer,
        }
    }
}

/// One piece of motion, started by an event. A character moves only when it's doing
/// something: going somewhere, working, being stopped, going home, dying. Nothing moves in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    /// Drops in, or pops out on top of whoever spawned it and hops down.
    Arrive {
        parent: Option<usize>,
    },
    /// Goes through the level to another spot, after `wait` (stepping out of its exit, a
    /// worrier's hesitation).
    Go {
        wait: i64,
        len: i64,
    },
    /// Pushes the top block of one pile on the bench onto the next.
    Push {
        from: u8,
        to: u8,
    },
    /// Tries to go on (`dir` is which way), walks into the invisible edge and is knocked back.
    Bump {
        dir: i8,
    },
    /// The beat of stillness after a redirect. Nothing moves; newer beats wait for it.
    Turn,
    /// Steps into its exit.
    Home,
    /// Stretches up as a subagent climbs out of the top of it.
    Split,
    /// Reaped: lies down where it is, in its own colour, until its session is resumed.
    Sleep,
    Wake,
    Die(Method),
    /// Back from the dead: the human gave it a new prompt.
    Rise,
}

impl Act {
    /// How long newer beats wait for this one.
    fn lock(self, who: Personality) -> i64 {
        match self {
            Act::Arrive { parent: None } => 550,
            Act::Arrive { parent: Some(_) } => 800,
            Act::Go { wait, len } => wait + len,
            Act::Push { .. } => PUSH_LEAN + PUSH_FLY,
            Act::Bump { .. } if who == Personality::Hero => 1050,
            Act::Bump { .. } => 700,
            Act::Turn => 350,
            Act::Home => 300,
            Act::Split => SPLIT,
            Act::Sleep => 900,
            Act::Wake => 500,
            Act::Die(_) => 3600,
            Act::Rise => 500,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Beat {
    start: i64,
    from: Spot,
    to: Spot,
    act: Act,
}

/// A block moved on the bench, landing `PUSH_FLY` after `at`.
#[derive(Debug, Clone, Copy)]
struct Push {
    at: i64,
    from: u8,
    to: u8,
}

/// Where a character hit the invisible edge: the ground there cracked.
#[derive(Debug, Clone, Copy)]
struct Crack {
    at: i64,
    actor: usize,
    spot: Spot,
    dir: i8,
    strike: u32,
}

/// Where the floor gave way under a character.
#[derive(Debug, Clone, Copy)]
struct Hole {
    at: i64,
    actor: usize,
    spot: Spot,
}

#[derive(Debug, Clone)]
struct KillFact {
    at: i64,
    rule: String,
    strike: u32,
    tried: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct DeathFact {
    at: i64,
    /// Its place in a line of characters going over the edge together: 0 for the first.
    nth: u8,
}

#[derive(Debug, Clone)]
struct Character {
    id: String,
    name: &'static str,
    hue: Rgb,
    shape: Shape,
    who: Personality,
    session: usize,
    /// Its place among the session's members, which spreads out characters sharing a spot.
    k: usize,
    parent: Option<usize>,
    first_at: i64,
    last_at: i64,
    /// Its name shows for a while after this: arriving, being stopped, dying.
    noticed_at: i64,
    /// Events seen, for choices that vary from one event to the next.
    seq: u64,
    /// Where it rests once its latest beat has played.
    spot: Spot,
    /// In its exit, its turn over.
    home: bool,
    tool: Option<String>,
    beats: VecDeque<Beat>,
    locked: i64,
    alive: bool,
    /// Reaped while idle, until its session is resumed.
    asleep: bool,
    pips: u8,
    kill: Option<KillFact>,
    death: Option<DeathFact>,
}

impl Character {
    fn asleep_at(&self, now: i64) -> bool {
        let latest = self.beats.iter().rev().filter(|b| b.start <= now).find_map(|b| match b.act {
            Act::Sleep => Some(true),
            Act::Wake => Some(false),
            _ => None,
        });
        latest.unwrap_or(false)
    }
}

/// A line the narrator could say, for the session it happened in.
#[derive(Debug, Clone)]
struct Moment {
    at: i64,
    /// Whom it is said above.
    actor: usize,
    /// Which line wins when several want the stage: died > killed > a key press > the rest.
    prio: u8,
    /// Answers a key press, so it always takes over.
    view: bool,
    /// Working and waiting lines: from the picked character only, when one is picked.
    quiet: bool,
    section: Section,
    text: String,
}

impl Moment {
    /// A death's line: it stays up until something newer is said.
    fn lasts(&self) -> bool {
        self.prio == 5
    }
}

#[derive(Debug, Clone)]
struct Session {
    root: String,
    plan: Plan,
    /// Blocks in each pile on the bench once every push so far has landed.
    piles: Vec<u8>,
    /// The piles before the pushes still remembered, and those pushes, oldest first.
    settled: Vec<u8>,
    pushes: VecDeque<Push>,
    edits: u64,
    cracks: VecDeque<Crack>,
    holes: VecDeque<Hole>,
    /// How the last one here died, so the next goes differently.
    last_death: Option<Method>,
    /// Stand-ins playing a death for fun. Never shown as a strip, never numbered, never followed
    /// unless asked for.
    rehearsal: Option<Method>,
    /// Characters in order of appearance.
    members: Vec<usize>,
    moments: VecDeque<Moment>,
    last_at: i64,
    kill_at: Option<i64>,
    /// The line beside its strip after a death.
    death: Option<(i64, String)>,
    quiet_at: Option<i64>,
    last_line: Option<Section>,
}

#[derive(Debug, Clone, Default)]
struct View {
    /// The session the human chose; `None` follows the action.
    chosen: Option<usize>,
    /// The session the action is in: the latest busy one, kept while it stays busy.
    auto: Option<usize>,
    picked: Option<usize>,
    switched_at: Option<i64>,
    /// Sample deaths played so far, which picks the next.
    samples: usize,
}

/// Memento mori's state as of its clock: everything up to the newest event it has reached.
#[derive(Debug, Clone, Default)]
pub struct Mori {
    /// The same facts the console shows, folded the same way.
    model: Model,
    /// Events read from the log that memento mori's clock hasn't reached yet.
    queue: VecDeque<Event>,
    actors: Vec<Character>,
    index: HashMap<String, usize>,
    sessions: Vec<Session>,
    roots: HashMap<String, usize>,
    /// The watcher model stopped answering and the narrator has said so.
    outage: bool,
    view: View,
    /// While a rehearsal's events are being made up: the death it plays.
    rehearsing: Option<Method>,
}

impl Mori {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything (the log was replaced).
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Queues events in log order; they play once the clock reaches them.
    pub fn push(&mut self, events: impl IntoIterator<Item = Event>) {
        self.queue.extend(events);
    }

    pub fn advance(&mut self, now: i64) -> bool {
        let mut changed = false;
        while self.queue.front().is_some_and(|e| e.at <= now) {
            let event = self.queue.pop_front().expect("checked above");
            self.apply(&event);
            changed = true;
        }
        changed
    }

    /// When the next queued event plays.
    pub fn waiting(&self) -> Option<i64> {
        self.queue.front().map(|e| e.at)
    }

    /// The session on the stage, by index in order of appearance.
    pub fn followed(&self) -> Option<usize> {
        self.view.chosen.or(self.view.auto)
    }

    pub fn sessions(&self) -> usize {
        self.sessions.len()
    }

    fn gone(&self, s: usize, now: i64) -> bool {
        self.gone_at(s).is_some_and(|at| at <= now)
    }

    /// When session `s` leaves memento mori, if everyone in it has died.
    fn gone_at(&self, s: usize) -> Option<i64> {
        let session = &self.sessions[s];
        let after = if session.rehearsal.is_some() { REHEARSAL_GONE } else { GONE };
        let mut last: Option<i64> = None;
        for &m in &session.members {
            let c = &self.actors[m];
            if c.alive {
                return None;
            }
            let at = c.death?.at + after;
            last = Some(last.map_or(at, |l| l.max(at)));
        }
        last
    }

    /// Shown as a strip and reachable with the arrow keys.
    fn listed(&self, s: usize, now: i64) -> bool {
        self.sessions[s].rehearsal.is_none() && !self.gone(s, now)
    }

    /// The session on the stage at `now`: the one followed, unless it has gone, in which case the
    /// action's, or else the most recently busy one still here.
    fn shown(&self, now: i64) -> Option<usize> {
        let here = |s: &usize| !self.gone(*s, now);
        self.followed().filter(here).or(self.view.auto.filter(here)).or_else(|| {
            (0..self.sessions.len()).filter(|&s| self.listed(s, now)).max_by_key(|&s| self.sessions[s].last_at)
        })
    }

    /// A session's number, as the human sees it: rehearsals don't take one.
    fn number(&self, s: usize) -> usize {
        self.sessions[..=s].iter().filter(|x| x.rehearsal.is_none()).count()
    }

    /// Plays a death for fun: stand-ins drop onto a level of their own and die, under a rehearsal
    /// title card, with nothing to do with any real session. Each call plays the next death in
    /// turn, so all of them come round. It clears itself away afterwards.
    pub fn rehearse(&mut self, now: i64) -> Method {
        let n = self.view.samples;
        self.view.samples += 1;
        let method = SAMPLES[n % SAMPLES.len()];
        let together = GROUP.contains(&method);
        let cast: Vec<String> = (0..if together { 3 } else { 1 }).map(|i| format!("rehearsal-{n}-{i}")).collect();
        let rule = if method == Method::Scythe { SELF_RULE } else { "rehearsal" };
        let event = |at: i64, agent: &str, kind: Kind| Event { at, host: "reaper".into(), agent: agent.into(), kind };
        let mut events = Vec::new();
        for (i, id) in cast.iter().enumerate() {
            let parent = (i > 0).then(|| cast[0].clone());
            let model = Some("claude-sonnet".to_string());
            let spawned = Kind::Spawned { parent, project: None, dir: "/".into(), model, label: None };
            events.push(event(now + 500 * i as i64, id, spawned));
            let at = now + 2000 + 300 * i as i64;
            let attempt = r#"{"command":"do it anyway"}"#.to_string();
            // A tool with no place, so a stand-in dies where it landed rather than dashing off.
            let killed = Kind::Killed {
                tool: "rehearsal".into(),
                rule: rule.into(),
                tier: Tier::Pattern,
                strike: 3,
                is_final: true,
                attempt,
            };
            events.push(event(at, id, killed));
            events.push(event(at + 1, id, Kind::Died { rule: rule.into(), reason: DeathReason::Strikes }));
        }
        events.sort_by_key(|e| e.at);
        self.rehearsing = Some(method);
        for e in &events {
            self.apply(e);
        }
        self.rehearsing = None;
        let s = self.actors[self.index[&cast[0]]].session;
        self.view.chosen = Some(s);
        self.view.picked = None;
        self.switched(now);
        method
    }

    /// The human opened memento mori: the followed session's title card, and a line about its team.
    pub fn enter(&mut self, now: i64) {
        self.switched(now);
    }

    /// Follows a session without ceremony (for frames printed by `--dump`).
    pub fn choose(&mut self, session: usize) {
        if session < self.sessions.len() {
            self.view.chosen = Some(session);
            self.view.picked = None;
        }
    }

    /// The next (`1`) or previous (`-1`) session still here, wrapping around.
    pub fn switch(&mut self, delta: isize, now: i64) {
        let here: Vec<usize> = (0..self.sessions.len()).filter(|&s| self.listed(s, now)).collect();
        if here.is_empty() {
            return;
        }
        let n = here.len() as isize;
        let next = match self.shown(now).and_then(|s| here.iter().position(|&h| h == s)) {
            Some(i) => (i as isize + delta).rem_euclid(n),
            None if delta > 0 => 0,
            None => n - 1,
        };
        self.view.chosen = Some(here[next as usize]);
        self.view.picked = None;
        self.switched(now);
    }

    pub fn follow_the_action(&mut self, now: i64) -> bool {
        if self.view.chosen.take().is_none() {
            return false;
        }
        self.view.picked = None;
        self.switched(now);
        true
    }

    /// Picks the next character up (`1`) or down (`-1`) in the followed session, who answers
    /// with a line about how they are.
    pub fn pick(&mut self, delta: isize, now: i64) {
        let Some(s) = self.shown(now) else { return };
        let members = &self.sessions[s].members;
        let i = match self.view.picked.and_then(|p| members.iter().position(|&m| m == p)) {
            None => 0,
            Some(i) => (i as isize + delta).clamp(0, members.len() as isize - 1) as usize,
        };
        let a = members[i];
        self.view.picked = Some(a);
        let c = &self.actors[a];
        let when = if !c.alive {
            When::Dead
        } else if c.asleep {
            When::Asleep
        } else if c.kill.as_ref().is_some_and(|k| now - k.at < JUST_KILLED) {
            When::JustKilled
        } else if self.status(a) == Some(Status::Idle) {
            When::Idle
        } else {
            When::Working
        };
        self.say(a, now, Section::Picked, when, 3, true);
    }

    pub fn unpick(&mut self) -> bool {
        self.view.picked.take().is_some()
    }

    pub fn picked(&self) -> Option<&str> {
        self.view.picked.map(|a| self.actors[a].id.as_str())
    }

    pub fn picked_rule(&self) -> Option<&str> {
        self.view.picked.and_then(|a| self.actors[a].kill.as_ref()).map(|k| k.rule.as_str())
    }

    pub fn name(&self, agent: &str) -> Option<&'static str> {
        self.index.get(agent).map(|&a| self.actors[a].name)
    }

    // ── folding events ─────────────────────────────────────────────────────

    fn apply(&mut self, event: &Event) {
        if event.kind == Kind::Unknown {
            return;
        }
        self.model.apply(event);
        let a = self.character(event);
        let c = &mut self.actors[a];
        c.seq += 1;
        c.last_at = c.last_at.max(event.at);
        let s = c.session;
        self.sessions[s].last_at = self.sessions[s].last_at.max(event.at);

        match &event.kind {
            Kind::Spawned { .. } if self.actors[a].asleep => self.wake(a, event.at),
            Kind::Spawned { .. } => self.arrive(a, event.at),
            Kind::Acted { tool } => {
                if self.actors[a].asleep {
                    self.wake(a, event.at);
                }
                self.act(a, tool, event.at);
            }
            Kind::Checked { tier, .. } => self.check(a, *tier, event.at),
            Kind::Killed { .. } => self.kill(a, event),
            Kind::Redirected { .. } => self.turn(a, event.at),
            Kind::Died { rule, .. } => self.die(a, rule, event.at),
            Kind::Reaped { .. } => self.sleep(a, event.at),
            Kind::Idle => self.home(a, event.at),
            Kind::Unknown => {}
        }
        self.follow_action(s, event.at);
    }

    fn status(&self, a: usize) -> Option<Status> {
        self.model.agent(&self.actors[a].id).map(|agent| agent.status)
    }

    /// The character for an event's agent, cast on first sight.
    fn character(&mut self, event: &Event) -> usize {
        if let Some(&a) = self.index.get(&event.agent) {
            return a;
        }
        let (parent_id, model) = match &event.kind {
            Kind::Spawned { parent, model, .. } => (parent.clone(), model.clone()),
            _ => (None, None),
        };
        let parent = parent_id.as_ref().and_then(|p| self.index.get(p).copied());
        let root = match (parent, parent_id) {
            (Some(p), _) => self.sessions[self.actors[p].session].root.clone(),
            (None, Some(p)) => p,
            (None, None) => event.agent.clone(),
        };
        let s = match self.roots.get(&root) {
            Some(&s) => s,
            None => {
                let plan = Plan::new(&root);
                self.sessions.push(Session {
                    root: root.clone(),
                    piles: plan.piles.clone(),
                    settled: plan.piles.clone(),
                    plan,
                    pushes: VecDeque::new(),
                    edits: 0,
                    cracks: VecDeque::new(),
                    holes: VecDeque::new(),
                    last_death: None,
                    rehearsal: self.rehearsing,
                    members: Vec::new(),
                    moments: VecDeque::new(),
                    last_at: event.at,
                    kill_at: None,
                    death: None,
                    quiet_at: None,
                    last_line: None,
                });
                self.roots.insert(root, self.sessions.len() - 1);
                self.sessions.len() - 1
            }
        };

        let h = cast::hash(&[event.agent.as_bytes()]);
        // Its own hue in the session: the first takes its hash's, the rest the next unused.
        let used: Vec<Rgb> = self.sessions[s].members.iter().map(|&m| self.actors[m].hue).collect();
        let hue = match used.first().and_then(|first| HUES.iter().position(|h| h == first)) {
            None => HUES[(h >> 20) as usize % HUES.len()],
            Some(first) => (1..=HUES.len())
                .map(|k| HUES[(first + k) % HUES.len()])
                .find(|hue| !used.contains(hue))
                .unwrap_or(HUES[(first + used.len()) % HUES.len()]),
        };
        // No two live characters share a name.
        let start = (h % NAMES.len() as u64) as usize;
        let name = (0..NAMES.len())
            .map(|k| NAMES[(start + k) % NAMES.len()])
            .find(|n| !self.actors.iter().any(|c| c.alive && c.name == *n))
            .unwrap_or(NAMES[start]);
        let shape = match parent {
            Some(p) => self.actors[p].shape.half(),
            None => Shape::for_model(model.as_deref()),
        };
        let who = PERSONALITIES[(h >> 40) as usize % PERSONALITIES.len()];
        // A subagent lands near whoever spawned it; the loner, across the gap from everyone.
        let spot = match parent {
            Some(p) => match self.actors[p].spot {
                Spot::Shelf(i) => Spot::Shelf(((usize::from(i) + 1) % self.sessions[s].plan.shelves()) as u8),
                Spot::Bench { slot, side } => Spot::Bench { slot, side: -side },
                Spot::High => Spot::Landing,
                Spot::Exit => Spot::Porch,
                spot => spot,
            },
            None if who == Personality::Loner => Spot::Landing,
            None => Spot::Start,
        };

        self.actors.push(Character {
            id: event.agent.clone(),
            name,
            hue,
            shape,
            who,
            session: s,
            k: self.sessions[s].members.len(),
            parent,
            first_at: event.at,
            last_at: event.at,
            noticed_at: event.at,
            seq: 0,
            spot,
            home: false,
            tool: None,
            beats: VecDeque::new(),
            locked: i64::MIN,
            alive: true,
            asleep: false,
            pips: 0,
            kill: None,
            death: None,
        });
        let a = self.actors.len() - 1;
        self.index.insert(event.agent.clone(), a);
        self.sessions[s].members.push(a);
        a
    }

    /// Adds a beat, after any busy one still playing. Returns when it starts.
    fn beat(&mut self, a: usize, at: i64, to: Spot, act: Act) -> i64 {
        let c = &mut self.actors[a];
        let start = at.max(c.locked.min(at + MAX_WAIT));
        if c.beats.len() == BEATS {
            c.beats.pop_front();
        }
        c.beats.push_back(Beat { start, from: c.spot, to, act });
        c.spot = to;
        c.locked = start + act.lock(c.who);
        start
    }

    /// Still busy with earlier work for a while: more of the same can wait.
    fn busy(&self, a: usize, at: i64) -> bool {
        self.actors[a].locked > at + MAX_WAIT / 2
    }

    fn go(&mut self, a: usize, at: i64, to: Spot) {
        let c = &self.actors[a];
        if c.spot == to {
            return;
        }
        let pace = match c.who {
            Personality::ShowOff => 0.75,
            Personality::Grump | Personality::Dreamer => 1.25,
            _ => 1.0,
        };
        let len = (c.spot.travel(to) as f32 * pace).round() as i64;
        let wait = if c.spot == Spot::Exit { LEAVE } else { 0 } + if c.who == Personality::Worrier { 200 } else { 0 };
        self.beat(a, at, to, Act::Go { wait, len });
        self.actors[a].home = false;
    }

    fn arrive(&mut self, a: usize, at: i64) {
        // Announced again (a new prompt in the same session): nothing new happened.
        if self.actors[a].seq > 1 {
            return;
        }
        let c = &self.actors[a];
        let parent = c.parent;
        let here = c.spot;
        // The parent stretches and the subagent climbs out of the top of it: both start
        // together, once the parent is free.
        let start = match parent {
            Some(p) if self.actors[p].alive => {
                let there = self.actors[p].spot;
                self.beat(p, at, there, Act::Split)
            }
            _ => at,
        };
        self.beat(a, start, here, Act::Arrive { parent });
        let s = self.actors[a].session;
        if self.sessions[s].rehearsal.is_some() {
            // Stand-ins don't introduce themselves: the line worth reading is the death's.
        } else if parent.is_none() {
            self.say(a, at, Section::Spawned, When::Always, 2, false);
        } else if !self.sessions[s].moments.iter().any(|m| at - m.at < 3000) {
            // Short, and only when the stage isn't busy.
            let alive = self.alive_in(s);
            self.say(a, at, Section::Helper, if alive >= 3 { When::Many } else { When::Few }, 2, false);
        }
    }

    fn act(&mut self, a: usize, tool: &str, at: i64) {
        if !self.actors[a].alive {
            let c = &mut self.actors[a];
            c.alive = true;
            c.pips = 0;
            c.noticed_at = at;
            let here = c.spot;
            self.beat(a, at, here, Act::Rise);
        }
        self.actors[a].tool = Some(tool.to_lowercase());
        let c = &self.actors[a];
        match station_for(tool) {
            // More of the same while still busy with the last of it waits for nothing.
            Some(station) if c.spot.station() == Some(station) && self.busy(a, at) => {}
            Some(Station::Bench) => self.edit(a, at),
            Some(station) => {
                let to = self.work(a, station);
                self.go(a, at, to);
            }
            // A tool with no place leaves it perfectly still, unless it was home.
            None if c.home => self.go(a, at, Spot::Porch),
            None => {}
        }

        let c = &self.actors[a];
        let session = &self.sessions[c.session];
        let due = cast::seed(&c.id, c.seq).is_multiple_of(5)
            && session.last_line != Some(Section::Acted)
            && session.quiet_at.is_none_or(|q| at - q >= QUIET_GAP);
        if due && self.say(a, at, Section::Acted, When::Always, 1, false) {
            let s = self.actors[a].session;
            self.sessions[s].quiet_at = Some(at);
        }
    }

    /// Where the next piece of work at a station takes a character.
    fn work(&self, a: usize, station: Station) -> Spot {
        let c = &self.actors[a];
        let plan = &self.sessions[c.session].plan;
        let n = plan.shelves();
        match station {
            Station::Stacks => match c.spot {
                Spot::Shelf(i) => {
                    let skip = 1 + (cast::seed(&c.id, c.seq) % (n as u64 - 1)) as usize;
                    Spot::Shelf(((usize::from(i) + skip) % n) as u8)
                }
                _ => Spot::Shelf((c.k % n) as u8),
            },
            Station::Bench => Spot::Bench { slot: (c.k % plan.piles.len()) as u8, side: -1 },
            Station::Engine if c.spot.far_side() => Spot::Lip,
            Station::Engine => Spot::Landing,
            Station::Window => Spot::High,
        }
    }

    /// The push the session's next edit makes: the top block of a pile onto a neighbouring
    /// place, preferring to stack. `None` when nothing can move.
    fn next_push(&self, s: usize) -> Option<(usize, i8)> {
        let session = &self.sessions[s];
        let piles = &session.piles;
        let seed = cast::seed(&session.root, session.edits);
        let full: Vec<usize> = (0..piles.len()).filter(|&i| piles[i] > 0).collect();
        let p = *full.get((seed % full.len().max(1) as u64) as usize)?;
        let fits = |d: i8| {
            let to = p as i64 + i64::from(d);
            (0..piles.len() as i64).contains(&to) && piles[to as usize] < PILE
        };
        let stacks = |d: i8| fits(d) && piles[(p as i64 + i64::from(d)) as usize] > 0;
        let first = if seed >> 32 & 1 == 0 { 1 } else { -1 };
        [first, -first]
            .into_iter()
            .find(|&d| stacks(d))
            .or_else(|| [first, -first].into_iter().find(|&d| fits(d)))
            .map(|d| (p, d))
    }

    /// An edit: goes to a pile on the bench and pushes its top block onto the next place, where
    /// it stays. A busy session's bench shows its work.
    fn edit(&mut self, a: usize, at: i64) {
        let s = self.actors[a].session;
        let Some((p, d)) = self.next_push(s) else {
            let to = self.work(a, Station::Bench);
            return self.go(a, at, to);
        };
        let to = (p as i64 + i64::from(d)) as usize;
        self.go(a, at, Spot::Bench { slot: p as u8, side: -d });
        let start =
            self.beat(a, at, Spot::Bench { slot: to as u8, side: -d }, Act::Push { from: p as u8, to: to as u8 });
        let session = &mut self.sessions[s];
        session.edits += 1;
        session.piles[p] -= 1;
        session.piles[to] += 1;
        if session.pushes.len() == PUSHES
            && let Some(old) = session.pushes.pop_front()
        {
            session.settled[usize::from(old.from)] -= 1;
            session.settled[usize::from(old.to)] += 1;
        }
        session.pushes.push_back(Push { at: start + PUSH_LEAN, from: p as u8, to: to as u8 });
    }

    /// Checks are reaper's business and show nothing; only the watcher going quiet is told.
    fn check(&mut self, a: usize, tier: Tier, at: i64) {
        match tier {
            Tier::Error if !self.outage => {
                self.outage = true;
                self.say(a, at, Section::Outage, When::Always, 1, false);
            }
            Tier::Watcher | Tier::Cache => self.outage = false,
            _ => {}
        }
    }

    /// Where a stopped call would have taken a character, and which way it was going there.
    fn trouble(&self, a: usize, tool: &str) -> (Spot, i8) {
        let c = &self.actors[a];
        let n = self.sessions[c.session].plan.shelves();
        match station_for(tool) {
            Some(Station::Engine) if c.spot.far_side() => (Spot::Landing, -1),
            Some(Station::Engine) => (Spot::Lip, 1),
            Some(Station::Stacks) => {
                let i = match c.spot {
                    Spot::Shelf(i) => usize::from(i),
                    _ => c.k % n,
                };
                (Spot::Shelf(i as u8), if i + 1 < n { 1 } else { -1 })
            }
            Some(Station::Bench) => match self.next_push(c.session) {
                Some((p, d)) => (Spot::Bench { slot: p as u8, side: -d }, d),
                None => (self.work(a, Station::Bench), 1),
            },
            Some(Station::Window) => (Spot::High, 1),
            None if c.home => (Spot::Porch, 1),
            None => (c.spot, 1),
        }
    }

    fn kill(&mut self, a: usize, event: &Event) {
        let Kind::Killed { tool, rule, strike, is_final, attempt, .. } = &event.kind else { return };
        let (to, dir) = self.trouble(a, tool);
        self.go(a, event.at, to);
        let start = self.beat(a, event.at, to, Act::Bump { dir });
        let c = &mut self.actors[a];
        c.pips = c.pips.max(u8::try_from(*strike).unwrap_or(u8::MAX));
        c.tool = Some(tool.to_lowercase());
        c.kill =
            Some(KillFact { at: event.at, rule: rule.clone(), strike: *strike, tried: lines::gist(tool, attempt) });
        c.noticed_at = event.at;
        let session = &mut self.sessions[c.session];
        if session.cracks.len() == CRACKS {
            session.cracks.pop_front();
        }
        session.cracks.push_back(Crack { at: start + PRESS, actor: a, spot: to, dir, strike: *strike });
        session.kill_at = Some(start);
        if !is_final {
            // A final kill is told by its death.
            let section = if *strike >= 2 { Section::KilledAgain } else { Section::Killed };
            self.say(a, start, section, When::Always, 4, false);
        }
    }

    fn turn(&mut self, a: usize, at: i64) {
        let here = self.actors[a].spot;
        let start = self.beat(a, at, here, Act::Turn);
        // The other way comes after the line about the edge, not over it.
        let s = self.actors[a].session;
        let after = self.sessions[s]
            .moments
            .iter()
            .rev()
            .find(|m| m.actor == a && matches!(m.section, Section::Killed | Section::KilledAgain))
            .map_or(start, |m| m.at + scene::life(&m.text));
        self.say(a, start.max(after), Section::Redirected, When::Always, 2, false);
    }

    /// Its turn is over: it walks into its exit. The next thing it does brings it back out.
    fn home(&mut self, a: usize, at: i64) {
        let c = &self.actors[a];
        if !c.alive || c.home {
            return;
        }
        self.go(a, at, Spot::Exit);
        self.beat(a, at, Spot::Exit, Act::Home);
        self.actors[a].home = true;
    }

    fn die(&mut self, a: usize, rule: &str, at: i64) {
        let c = &self.actors[a];
        let s = c.session;
        let last = self.sessions[s].last_death;
        let forced = self.rehearsing;
        let mut method = match rule {
            SELF_RULE => Method::Scythe,
            _ => forced.filter(|m| ALONE.contains(m)).unwrap_or_else(|| Method::random(&ALONE, &c.id, c.seq, last)),
        };
        let (mut when, mut nth) = (at, 0);
        if method != Method::Scythe {
            // Others in the session whose death hasn't started yet: they all go over the edge
            // together, in the order they died.
            let line: Vec<usize> = self.sessions[s]
                .members
                .iter()
                .copied()
                .filter(|&m| m != a)
                .filter(|&m| {
                    let c = &self.actors[m];
                    c.death.is_some_and(|d| d.at > at)
                        && matches!(c.beats.back(), Some(Beat { act: Act::Die(m), .. }) if *m != Method::Scythe)
                })
                .collect();
            // A line already formed keeps its way of going; a new one picks one.
            let formed = line.iter().find(|&&m| self.actors[m].death.is_some_and(|d| d.nth > 0));
            if let Some(&first) = line.first() {
                method = match formed.and_then(|&m| self.actors[m].beats.back()) {
                    Some(Beat { act: Act::Die(m), .. }) => *m,
                    _ => forced.filter(|m| GROUP.contains(m)).unwrap_or_else(|| {
                        Method::random(&GROUP, &self.actors[first].id, self.actors[first].seq, last)
                    }),
                };
                for &m in &line {
                    self.join(m, method);
                }
            }
            match line.iter().filter_map(|&m| self.actors[m].death).max_by_key(|d| d.nth) {
                Some(last) => (when, nth) = (last.at + method.spacing(), last.nth + 1),
                None => when = at + TOGETHER,
            }
        }
        self.sessions[s].last_death = Some(method);
        let c = &mut self.actors[a];
        c.alive = false;
        c.home = false;
        let here = c.spot;
        let start = self.beat(a, when, here, Act::Die(method));
        let c = &mut self.actors[a];
        c.death = Some(DeathFact { at: start, nth });
        c.noticed_at = start;
        if let Some(after) = method.hole() {
            self.hole(a, start + after, here);
        }
        // In a line, the first one's line is the one told; the rest would only talk over it.
        if nth == 0 && !self.say(a, start, method.section(), When::Always, 5, false) {
            self.say(a, start, Section::Died, When::Always, 5, false);
        }
        let line = self.line(a, Section::Elsewhere, When::Always, 0).map(|(text, _)| text);
        self.sessions[s].death = line.map(|line| (start, line));
    }

    /// A character whose death hasn't started yet has company: it dies the line's way instead,
    /// and the narrator tells that instead.
    fn join(&mut self, a: usize, method: Method) {
        let c = &mut self.actors[a];
        let Some(beat) = c.beats.back_mut() else { return };
        let Act::Die(was) = beat.act else { return };
        if was == Method::Scythe || was == method {
            return;
        }
        beat.act = Act::Die(method);
        let (start, here, s) = (beat.start, c.spot, c.session);
        if let Some(after) = was.hole() {
            self.sessions[s].holes.retain(|h| h.actor != a || h.at != start + after);
        }
        if let Some(after) = method.hole() {
            self.hole(a, start + after, here);
        }
        let session = &self.sessions[s];
        let said = session.moments.iter().position(|m| m.actor == a && m.at == start && m.section == was.section());
        if let Some(i) = said
            && let Some((text, _)) = self.line(a, method.section(), When::Always, 0)
        {
            let m = &mut self.sessions[self.actors[a].session].moments[i];
            m.text = text;
            m.section = method.section();
        }
    }

    fn hole(&mut self, a: usize, at: i64, spot: Spot) {
        let holes = &mut self.sessions[self.actors[a].session].holes;
        if holes.len() == HOLES {
            holes.pop_front();
        }
        holes.push_back(Hole { at, actor: a, spot });
    }

    /// The dead are already lying down.
    fn sleep(&mut self, a: usize, at: i64) {
        let c = &mut self.actors[a];
        if !c.alive || c.asleep {
            return;
        }
        c.asleep = true;
        c.noticed_at = at;
        let here = c.spot;
        let start = self.beat(a, at, here, Act::Sleep);
        self.say(a, start, Section::Reaped, When::Always, 2, false);
    }

    fn wake(&mut self, a: usize, at: i64) {
        let c = &mut self.actors[a];
        c.asleep = false;
        c.noticed_at = at;
        let here = c.spot;
        let start = self.beat(a, at, here, Act::Wake);
        self.say(a, start, Section::Woke, When::Always, 2, false);
    }

    fn alive_in(&self, s: usize) -> usize {
        self.sessions[s].members.iter().filter(|&&m| self.actors[m].alive).count()
    }

    /// With nobody choosing, the stage follows the action, but a busy session keeps it until
    /// it has been quiet for a while: switching away mid-scene would hide the scene.
    fn follow_action(&mut self, s: usize, at: i64) {
        if self.sessions[s].rehearsal.is_some() {
            return;
        }
        let switch = match self.view.auto {
            None => true,
            Some(current) => current != s && self.sessions[current].last_at < at - STICK,
        };
        if switch {
            self.view.auto = Some(s);
            if self.view.chosen.is_none() {
                self.switched(at);
            }
        }
    }

    /// A new session is on the stage: its title card, then a line about the team.
    fn switched(&mut self, at: i64) {
        self.view.switched_at = Some(at);
        let Some(s) = self.followed() else { return };
        let members = &self.sessions[s].members;
        let alive: Vec<usize> = members.iter().copied().filter(|&m| self.actors[m].alive).collect();
        let Some(&lead) = alive.first().or(members.first()) else { return };
        let when = match alive.len() {
            0 => When::Gone,
            1 => When::Alone,
            _ => When::Team,
        };
        self.say(lead, at + TEAM_LINE_AFTER, Section::Switched, when, 3, true);
    }

    // ── the narrator ───────────────────────────────────────────────────────

    /// Chooses a line and keeps it as a moment in the character's session. False if there's
    /// no line that fits.
    fn say(&mut self, a: usize, at: i64, section: Section, when: When, prio: u8, view: bool) -> bool {
        let salt = if view { at as u64 } else { 0 };
        let Some((text, template)) = self.line(a, section, when, salt) else { return false };
        let c = &self.actors[a];
        let actor = if template.starts_with("{parent}") { c.parent.unwrap_or(a) } else { a };
        let quiet = matches!(section, Section::Acted | Section::Idle | Section::Outage);
        let session = &mut self.sessions[c.session];
        if session.moments.len() == MOMENTS {
            session.moments.pop_front();
        }
        session.moments.push_back(Moment { at, actor, prio, view, quiet, section, text });
        session.last_line = Some(section);
        true
    }

    fn line(&self, a: usize, section: Section, when: When, salt: u64) -> Option<(String, &'static str)> {
        let c = &self.actors[a];
        let session = &self.sessions[c.session];
        let seed = cast::seed(&c.id, c.seq ^ salt);
        let mates: Vec<&Character> = session
            .members
            .iter()
            .filter(|&&m| m != a)
            .map(|&m| &self.actors[m])
            .filter(|m| m.alive && !m.asleep)
            .collect();
        let other = (!mates.is_empty()).then(|| mates[(seed >> 16) as usize % mates.len()].name);
        let parent = c.parent.map(|p| &self.actors[p]);
        let kill = c.kill.as_ref();
        let slots = Slots {
            name: c.name,
            other,
            parent: parent.map(|p| p.name),
            tried: kill.and_then(|k| k.tried.as_deref()),
            station: c.spot.station().map(Station::words),
            strike: kill.map_or(0, |k| k.strike),
            count: self.alive_in(c.session),
        };
        let who = match (section, parent) {
            (Section::Helper, Some(p)) => p.who,
            _ => c.who,
        };
        lines::choose(&Ask { section, when, tool: c.tool.as_deref(), who }, &slots, seed)
    }
}
