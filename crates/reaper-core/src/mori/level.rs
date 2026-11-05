//! The level a session plays on. Every session gets its own, from its id: shelves, loose
//! blocks, a ledge over a gap, then steps up to a high ledge with the exits beyond. The parts
//! always come in that order, so the space is learned once; their sizes differ, so each
//! session looks like its own place. Nothing on screen names any of it.
//!
//! [`Plan`] is the part that doesn't depend on memento mori's size; [`Level`] lays it out for one
//! size, as [`Fit`] fits it to the screen. Coordinates are pixels, as in the scene: one per
//! column, two per row. Land is solid from its top down, and its tops sit on cell boundaries so
//! it stays crisp in a terminal.

use super::Station;
use super::cast;

/// The most blocks one pile on the bench holds.
pub(super) const PILE: u8 = 3;
/// Columns between the centres of neighbouring exits: room for the widest body and its frame.
const EXIT_PITCH: i32 = 7;
/// How far above the plateau a second row of exits floats.
const EXIT_ROW: i32 = 10;
/// Columns of stairwell between one floor and the next, off the screen: whatever runs past the
/// end of a floor (a pack coming in, a saucer leaving) never turns up on the next.
const STAIRS: i32 = 48;
/// What going up or down the stairwell costs a trip, against a column of walking.
const CLIMB: f32 = 8.0;
/// The fewest rows a floor gets.
const BAND: i32 = 16;

/// Each resolves to a column on the level at any size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// The left end, where a newcomer drops in.
    Start,
    /// On top of a shelf: reading.
    Shelf(u8),
    /// Beside a pile on the bench, on the side it pushes from: editing.
    Bench {
        slot: u8,
        side: i8,
    },
    /// At the ledge's edge, over the gap: commands, from this side.
    Lip,
    /// Just across the gap: commands, from the far side.
    Landing,
    /// On the high ledge: the web.
    High,
    /// The middle of the stage, where deaths play.
    Centre,
    /// Just outside its own exit.
    Porch,
    Exit,
}

impl Spot {
    /// The place's name in the narrator's mouth.
    pub fn station(self) -> Option<Station> {
        match self {
            Spot::Shelf(_) => Some(Station::Stacks),
            Spot::Bench { .. } => Some(Station::Bench),
            Spot::Lip | Spot::Landing => Some(Station::Engine),
            Spot::High => Some(Station::Window),
            Spot::Start | Spot::Centre | Spot::Porch | Spot::Exit => None,
        }
    }

    /// Across the gap from where everyone starts.
    pub fn far_side(self) -> bool {
        matches!(self, Spot::Landing | Spot::High | Spot::Porch | Spot::Exit)
    }

    /// How far along the level it is, in quarter steps. Sets how long getting between two
    /// places takes, whatever memento mori's size.
    fn along(self) -> i64 {
        match self {
            Spot::Start => 0,
            Spot::Shelf(i) => 4 + i64::from(i),
            Spot::Bench { slot, .. } => 9 + i64::from(slot) / 2,
            Spot::Centre => 12,
            Spot::Lip => 13,
            Spot::Landing => 17,
            Spot::High => 20,
            Spot::Porch => 23,
            Spot::Exit => 24,
        }
    }

    /// Milliseconds to get from here to `to` at an ordinary pace.
    pub fn travel(self, to: Spot) -> i64 {
        let steps = (self.along() - to.along()).abs();
        match (self, to) {
            // One shelf to the next is a single hop.
            (Spot::Shelf(_), Spot::Shelf(_)) => 320,
            _ => 260 + steps * 55,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Plan {
    /// Each shelf's width in pixels and height in rows.
    shelves: Vec<(i32, i32)>,
    /// How many blocks each place on the bench starts with.
    pub piles: Vec<u8>,
    block: i32,
    /// Heights in rows above the ground.
    ledge: i32,
    landing: i32,
    steps: i32,
    /// Widths in pixels.
    gap: i32,
    tread: i32,
    high: i32,
}

impl Plan {
    pub fn new(id: &str) -> Plan {
        let h = cast::hash(&[b"level", id.as_bytes()]);
        let pick = |shift: u32, n: u64| ((h >> shift) % n) as i32;
        let shelves = (0..2 + pick(0, 3) as u32).map(|i| (3 + pick(3 + 2 * i, 3), 2 + pick(11 + 2 * i, 4))).collect();
        let mut piles = vec![1u8; 5];
        // Three or four blocks: one or two places start empty.
        for i in 0..1 + pick(20, 2) as u32 {
            let mut at = pick(22 + 3 * i, 5) as usize;
            while piles[at] == 0 {
                at = (at + 1) % piles.len();
            }
            piles[at] = 0;
        }
        Plan {
            shelves,
            piles,
            block: 2 + pick(30, 2),
            ledge: 1 + pick(32, 2),
            landing: 1 + pick(34, 2),
            steps: 2 + pick(36, 2),
            gap: 6 + pick(38, 3),
            tread: 2 + pick(41, 2),
            high: 4 + pick(43, 3),
        }
    }

    pub fn shelves(&self) -> usize {
        self.shelves.len()
    }
}

/// How the level fits the screen. A big terminal gets pixels twice the size. A tall, narrow one
/// gets the level folded into floors stacked up the screen, each running the other way from the
/// one below, so walking off the end of one comes out at the same side of the next. The level is
/// still one strip, with a stairwell off the screen at each fold; only drawing it uses the fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// Screen pixels to a level pixel, each way.
    pub zoom: i32,
    pub floors: i32,
    /// Each floor's width in level pixels.
    pub width: i32,
    /// Each floor's height in rows of level pixels.
    pub band: i32,
    /// Screen pixel rows above the top floor.
    above: i32,
}

impl Fit {
    /// For `cols` × `rows` cells, the strips' row included.
    pub fn new(cols: u16, rows: u16) -> Fit {
        let (cols, rows) = (i32::from(cols), i32::from(rows));
        // A level reads best about three and a half times as wide as it is tall.
        let floors = |w: i32, h: i32, least: i32| {
            let off = |f: i32| ((f * f * w) as f32 / h.max(1) as f32 / 3.5).ln().abs();
            (1..=3).filter(|&f| (f == 1 || h / f >= BAND) && f * w >= least).min_by(|&a, &b| off(a).total_cmp(&off(b)))
        };
        // Twice the size, once that still leaves as much room as an ordinary terminal, with a level
        // at least as long as one.
        let (w, h) = (cols / 2, rows / 2);
        let (zoom, w, h, floors) = match floors(w, h, 80) {
            Some(f) if w * h >= 80 * 22 && h >= BAND => (2, w, h, f),
            _ => (1, cols, rows, floors(cols, rows, 0).unwrap_or(1)),
        };
        let band = h / floors;
        Fit { zoom, floors, width: w, band, above: 2 * (rows - floors * band * zoom) }
    }

    /// Floor `k` starts at this column of the level.
    fn start(&self, k: i32) -> i32 {
        k * (self.width + STAIRS)
    }

    /// The floor level column `x` is on, and its column there, counted across the screen; `None`
    /// in a stairwell or past either end.
    fn place(&self, x: i32) -> Option<(i32, i32)> {
        let k = x.div_euclid(self.width + STAIRS);
        let along = x - self.start(k);
        if !(0..self.floors).contains(&k) || !(0..self.width).contains(&along) {
            return None;
        }
        Some((k, if k % 2 == 1 { self.width - 1 - along } else { along }))
    }

    /// The screen pixel row of floor `k`'s top. Floor 0 is at the bottom.
    fn top(&self, k: i32) -> i32 {
        self.above + 2 * self.zoom * self.band * (self.floors - 1 - k)
    }

    /// The top left screen pixel of level pixel (`x`, `y`), which takes `zoom` × `zoom` of them.
    pub fn pixel(&self, x: i32, y: i32) -> Option<(i32, i32)> {
        let (k, col) = self.place(x)?;
        Some((col * self.zoom, self.top(k) + y * self.zoom))
    }

    /// The screen cell for text at column `x` and cell row `row` of the level: in the middle of
    /// that column, on the lower of the rows it takes.
    pub fn cell(&self, x: i32, row: i32) -> Option<(i32, i32)> {
        let (k, col) = self.place(x)?;
        Some((col * self.zoom + self.zoom / 2, (self.top(k) + (2 * row + 2) * self.zoom) / 2 - 1))
    }
}

#[derive(Debug, Clone)]
pub(super) struct Level {
    pub fit: Fit,
    /// The whole strip, stairwells included.
    pub cols: i32,
    /// The lowest pixel row.
    pub floor: i32,
    /// The top of the ground everything stands on.
    pub ground: i32,
    /// The top pixel row of the land in each column.
    pub top: Vec<i32>,
    /// Room for everyone's name tags.
    pub names: bool,
    /// Each shelf's first column and width.
    shelves: Vec<(i32, i32)>,
    /// The first column of each place on the bench.
    slots: Vec<i32>,
    pub block: i32,
    /// The last column of the ledge.
    lip: i32,
    /// The first column across the gap.
    landing: i32,
    /// The high ledge's first column and width.
    high: (i32, i32),
    /// The plateau with the exits on it: its first column and the pixel row exits stand on.
    exits: (i32, i32),
    /// The middle of the stage, where deaths play.
    centre: i32,
}

impl Level {
    /// `cols` × `rows` are cells. Row 0 is the strips; the land leaves
    /// sky above it for the title, a line of narration, a name and the tallest character.
    /// Small terminals get flatter levels and lose the name tags. On floors, each floor is laid
    /// out the same way and the folds come where the land is flat: between the bench and the
    /// ledge, and along the landing.
    pub fn new(plan: &Plan, cols: u16, rows: u16) -> Level {
        let fit = Fit::new(cols, rows);
        let (fw, rows) = (fit.width, fit.band);
        let w = fit.start(fit.floors) - STAIRS;
        let floor = (2 * rows - 1).max(0);
        let thick = match rows {
            18.. => 6,
            10.. => 4,
            _ => 2,
        };
        let ground = (2 * rows - thick).max(2);
        // Taller terminals get taller levels, up to half again the plan's heights.
        let room = (ground / 2 - 8).max(0);
        let scale = (room as f32 / 5.0).min(1.5);
        let lift = |cells: i32| ground - 2 * ((cells as f32 * scale).round() as i32).clamp(0, room);
        // `f` of the way across floor `k`.
        let at = |k: i32, f: f32| fit.start(k) + (fw as f32 * f).round() as i32;
        let mut top = vec![ground; w.max(0) as usize];
        let mut fill = |from: i32, to: i32, y: i32| {
            for x in from.max(0)..to.min(w) {
                top[x as usize] = y;
            }
        };
        // Each part's floor, and how far across it the part runs. Deaths play in the middle of the
        // bottom floor, with open ground either side.
        let (stacks, bench, ledge, steps_on, centre) = match fit.floors {
            1 => ((0, 0.04, 0.22), (0, 0.25, 0.45), (0, 0.47, 0.58), 0, fw / 2),
            2 => ((0, 0.04, 0.38), (0, 0.58, 0.96), (1, 0.03, 0.22), 1, at(0, 0.48)),
            _ => ((0, 0.04, 0.38), (0, 0.58, 0.96), (1, 0.06, 0.45), 2, at(0, 0.48)),
        };

        let (a, b) = (at(stacks.0, stacks.1) + 1, at(stacks.0, stacks.2));
        let per = ((b - a) / plan.shelves.len() as i32).max(1);
        let shelves: Vec<(i32, i32)> = plan
            .shelves
            .iter()
            .enumerate()
            .map(|(i, &(sw, rise))| {
                let sw = sw.min(per - 1).max(1);
                let x = a + i as i32 * per + (per - sw) / 2;
                fill(x, x + sw, lift(rise));
                (x, sw)
            })
            .collect();

        // Blocks shrink to fit a narrow bench rather than spill onto the ledge.
        let (a, b) = (at(bench.0, bench.1), at(bench.0, bench.2));
        let pitch = ((b - a) / plan.piles.len() as i32).max(2);
        let block = plan.block.min(pitch - 1);
        let slots = (0..plan.piles.len() as i32).map(|i| a + i * pitch + (pitch - block) / 2).collect();

        let gap_at = at(ledge.0, ledge.2);
        let gap = (plan.gap + (fw - 80) / 20).clamp(3, 14);
        fill(at(ledge.0, ledge.1), gap_at, lift(plan.ledge));
        fill(gap_at, gap_at + gap, floor);
        let landing = gap_at + gap;
        fill(landing, w, lift(plan.landing));
        let first_step = fw / 40 + if steps_on == ledge.0 { landing + 3 } else { at(steps_on, 0.06) };
        let tread = plan.tread + fw / 80;
        for j in 1..=plan.steps {
            let x = first_step + (j - 1) * tread;
            fill(x, x + tread, lift(plan.landing + j));
        }
        let high = (first_step + plan.steps * tread, plan.high + fw / 40);
        fill(high.0, high.0 + high.1, lift(plan.landing + plan.steps + 1));

        Level {
            fit,
            cols: w,
            floor,
            ground,
            top,
            names: fit.floors * fw >= 70 && rows >= 18,
            shelves,
            slots,
            block,
            lip: gap_at - 1,
            landing,
            high,
            exits: (high.0 + high.1, lift(plan.landing) - 1),
            centre,
        }
    }

    /// The first column of the floor `x` is on, and the first past it: the screen's edges there.
    pub fn span(&self, x: i32) -> (i32, i32) {
        let k = x.div_euclid(self.fit.width + STAIRS).clamp(0, self.fit.floors - 1);
        (self.fit.start(k), self.fit.start(k) + self.fit.width)
    }

    /// The columns on the screen: every floor's, without the stairwells.
    pub fn shown(&self) -> i32 {
        self.fit.floors * self.fit.width
    }

    /// How long a leg takes, in arbitrary units: a jump costs more than a step, and a stairwell
    /// costs a few steps, however long it is off the screen.
    pub fn cost(&self, leg: Leg) -> f32 {
        match leg {
            Leg::Walk { from, to } => {
                let stairs = (from.min(to)..from.max(to)).filter(|&x| self.fit.place(x).is_none()).count() as i32;
                ((to - from).abs() - stairs) as f32 + ((stairs + STAIRS - 1) / STAIRS) as f32 * CLIMB
            }
            Leg::Jump { from, to, .. } => 5.0 + (to.0 - from.0).abs() as f32 * 0.6,
        }
    }

    /// The gap's first column and the first column past it.
    pub fn gap(&self) -> (i32, i32) {
        (self.lip + 1, self.landing)
    }

    pub fn slot(&self, i: usize) -> i32 {
        self.slots[i % self.slots.len()]
    }

    /// The column a character `w` wide stands at in `spot`. `k` is its place in its session,
    /// which spreads out characters sharing a spot.
    pub fn x(&self, spot: Spot, k: usize, w: i32) -> i32 {
        let spread = (k % 3) as i32 * (w + 1);
        let x = match spot {
            Spot::Start => 1 + spread,
            Spot::Shelf(i) => {
                let (x, sw) = self.shelves[usize::from(i) % self.shelves.len()];
                x + sw / 2 - w / 2
            }
            Spot::Bench { slot, side } => {
                let x = self.slot(usize::from(slot));
                if side < 0 { x - w } else { x + self.block }
            }
            Spot::Lip => self.lip + 1 - w - spread,
            Spot::Landing => self.landing + spread,
            Spot::High => self.high.0 + (self.high.1 - w) / 2,
            // The first dead centre, the rest either side of it.
            Spot::Centre => self.centre - w / 2 + [0, -1, 1][k % 3] * (w + 1),
            Spot::Exit => self.exit(k, w).0,
            Spot::Porch => self.exit(k, w).0 - w - 1,
        };
        x.clamp(0, (self.cols - w).max(0))
    }

    /// Where a character's exit is: its first column and the pixel row it stands on.
    pub fn exit(&self, k: usize, w: i32) -> (i32, i32) {
        let fit = ((self.cols - 1 - self.exits.0) / EXIT_PITCH).max(1) as usize;
        let centre = self.cols - 1 - EXIT_PITCH / 2 - (k % fit) as i32 * EXIT_PITCH;
        (centre - w / 2, self.exits.1 - (k / fit) as i32 * EXIT_ROW)
    }

    /// The pixel row a character `w` wide at column `x` stands on.
    pub fn stand(&self, land: &[i32], x: i32, w: i32) -> i32 {
        (x.max(0)..(x + w).min(self.cols)).map(|c| land[c as usize]).min().unwrap_or(self.ground) - 1
    }

    /// The way from column `from` to column `to` for a character `w` wide: walks along the
    /// land, with a jump wherever the height changes and over any gap or pit.
    pub fn route(&self, land: &[i32], from: i32, to: i32, w: i32) -> Vec<Leg> {
        let mut legs = Vec::new();
        let dir = (to - from).signum();
        let feet = |x: i32| self.stand(land, x, w);
        let past = |x: i32| dir * (x - to) > 0;
        let (mut x, mut walk) = (from, from);
        for _ in 0..4 * self.cols + 16 {
            if x == to {
                break;
            }
            let (y, ny) = (feet(x), feet(x + dir));
            if (ny - y).abs() <= 1 {
                x += dir;
                continue;
            }
            if x != walk {
                legs.push(Leg::Walk { from: walk, to: x });
            }
            // A drop of more than a row with land again a little way on, about as high, is a pit:
            // jump it. A deep drop is the gap: jump to whatever is across it.
            let ahead = || (2..=16).map(|d| x + d * dir).take_while(|&a| !past(a));
            let over = (ny > y + 2)
                .then(|| {
                    ahead()
                        .find(|&a| feet(a) <= y + 2)
                        .or_else(|| (ny > y + 8).then(|| ahead().find(|&a| feet(a) <= y + 8)).flatten())
                })
                .flatten();
            let (mut land_x, arc) = match over {
                Some(a) => (a, 3),
                None if ny < y => (x + dir, (y - ny) / 2 + 2),
                None => (x + dir, 0),
            };
            // Land with the whole body on the new ground, not teetering at its edge.
            let ly = feet(land_x);
            for _ in 0..w / 2 + 1 {
                if land_x == to || feet(land_x + dir) != ly || past(land_x + dir) {
                    break;
                }
                land_x += dir;
            }
            legs.push(Leg::Jump { from: (x, y), to: (land_x, ly), arc });
            x = land_x;
            walk = x;
        }
        if x != walk {
            legs.push(Leg::Walk { from: walk, to: x });
        }
        legs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Leg {
    Walk {
        from: i32,
        to: i32,
    },
    /// Through the air between two (column, row) footholds, rising `arc` pixels above the line between them.
    Jump {
        from: (i32, i32),
        to: (i32, i32),
        arc: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(id: &str, cols: u16, rows: u16) -> Level {
        Level::new(&Plan::new(id), cols, rows)
    }

    #[test]
    fn every_session_gets_its_own_level_with_the_parts_in_the_same_order() {
        let (a, b) = (level("session-a", 80, 24), level("session-b", 100, 30));
        assert_ne!(a.top, b.top, "different places");
        assert_eq!(a.top, level("session-a", 80, 24).top, "the same place every time");
        let (tall, taller) = (level("session-a", 44, 58), level("session-b", 30, 88));
        for l in [&a, &b, &tall, &taller] {
            let (shelf, slot) = (l.shelves[0].0, l.slot(0));
            assert!(shelf < slot && slot < l.lip && l.lip < l.landing && l.landing < l.high.0, "{l:?}");
            let bench_end = l.slot(4) + l.block;
            assert!(l.top[bench_end as usize] == l.ground, "the bench stays on the ground: {l:?}");
            assert!(l.high.0 < l.exits.0);
            assert!(l.top.iter().all(|y| y % 2 == 0 || *y == l.floor), "tops on cell boundaries");
            assert_eq!(l.top[l.lip as usize + 1], l.floor, "a gap after the ledge");
            assert!(l.top[l.high.0 as usize] < l.top[l.landing as usize], "steps up to the high ledge");
        }
    }

    #[test]
    fn tall_terminals_get_floors_and_big_ones_bigger_pixels() {
        let fit = |cols, rows| {
            let f = Fit::new(cols, rows);
            (f.zoom, f.floors)
        };
        for (cols, rows) in [(80, 22), (130, 26), (200, 12), (60, 30)] {
            assert_eq!(fit(cols, rows), (1, 1), "{cols}x{rows}");
            let f = Fit::new(cols, rows);
            assert_eq!((f.pixel(5, 7), f.cell(5, 3)), (Some((5, 7)), Some((5, 3))), "drawn as laid out");
        }
        assert_eq!(fit(44, 58), (1, 2));
        assert_eq!(fit(80, 48), (1, 2));
        assert_eq!(fit(30, 88), (1, 3));
        assert_eq!(fit(220, 58), (2, 1));
        assert_eq!(fit(120, 68), (2, 2), "zoomed only with a level as long as an ordinary one");
    }

    #[test]
    fn floors_fold_where_the_land_is_flat_and_run_back_and_forth() {
        for (id, cols, rows) in [("session-a", 44, 58), ("session-b", 30, 88), ("session-c", 120, 68)] {
            let l = level(id, cols, rows);
            let fit = l.fit;
            for k in 1..fit.floors {
                // The last column of the floor below, and the first of this one.
                let (end, start) = (fit.start(k) - STAIRS - 1, fit.start(k));
                assert!((end..=start).all(|x| l.top[x as usize] == l.top[end as usize]), "flat through the stairwell");
                assert!((end + 1..start).all(|x| fit.pixel(x, 0).is_none()), "the stairwell is off the screen");
                let (below, above) = (fit.pixel(end, 0).unwrap(), fit.pixel(start, 0).unwrap());
                assert_eq!(below.0, above.0, "out at the same side");
                assert_eq!(above.1, below.1 - 2 * fit.zoom * fit.band, "a floor up");
                let walk = Leg::Walk { from: end - 3, to: start + 3 };
                assert!(l.cost(walk) < 20.0, "the stairs take a few steps: {}", l.cost(walk));
            }
            let spots = [Spot::Start, Spot::Shelf(1), Spot::Bench { slot: 2, side: -1 }, Spot::Lip, Spot::Landing];
            for spot in spots.into_iter().chain([Spot::High, Spot::Exit, Spot::Centre]) {
                assert!(fit.place(l.x(spot, 0, 3)).is_some(), "{spot:?} is on a floor");
            }
            let (_, top) = fit.pixel(l.x(Spot::Exit, 0, 3), l.exits.1).unwrap();
            let (_, bottom) = fit.pixel(l.x(Spot::Start, 0, 3), l.ground).unwrap();
            assert!(top < bottom, "the exits are up at the top");
            let legs = l.route(&l.top, l.x(Spot::Start, 0, 3), l.x(Spot::Exit, 0, 3), 3);
            assert!(legs.iter().all(|leg| !matches!(leg, Leg::Jump { from, to, .. } if (from.1 - to.1).abs() > 12)));
        }
    }

    #[test]
    fn routes_walk_the_land_and_jump_the_gap() {
        let l = level("session-a", 100, 30);
        let land = l.top.clone();
        let (from, to) = (l.x(Spot::Lip, 0, 3), l.x(Spot::Landing, 0, 3));
        let legs = l.route(&land, from, to, 3);
        assert!(
            legs.iter()
                .any(|leg| matches!(leg, Leg::Jump { from, to, arc: 3 } if from.0 <= l.lip && to.0 >= l.landing)),
            "{legs:?}"
        );
        let mut at = from;
        for leg in &legs {
            let (a, b) = match *leg {
                Leg::Walk { from, to } => (from, to),
                Leg::Jump { from, to, .. } => (from.0, to.0),
            };
            assert_eq!(a, at, "{legs:?}");
            at = b;
        }
        assert_eq!(at, to);
        let back = l.route(&land, l.x(Spot::Exit, 0, 3), l.x(Spot::Start, 0, 3), 3);
        assert!(back.iter().filter(|leg| matches!(leg, Leg::Jump { .. })).count() >= 3, "{back:?}");
    }

    #[test]
    fn tiny_moris_still_lay_out() {
        for (cols, rows) in [(0, 0), (1, 1), (5, 3), (20, 5), (40, 10), (300, 90)] {
            let l = level("x", cols, rows);
            for spot in [Spot::Start, Spot::Shelf(3), Spot::Bench { slot: 4, side: 1 }, Spot::Lip, Spot::Exit] {
                let x = l.x(spot, 7, 4);
                let _ = l.route(&l.top, 0, x, 4);
            }
        }
    }
}
