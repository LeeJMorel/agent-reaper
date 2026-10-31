//! The level a session plays on. Every session gets its own, from its id: shelves, loose
//! blocks, a ledge over a gap, then steps up to a high ledge with the exits beyond. The parts
//! always come in that order, so the space is learned once; their sizes differ, so each
//! session looks like its own place. Nothing on screen names any of it.
//!
//! [`Plan`] is the part that doesn't depend on memento mori's size; [`Level`] lays it out for one
//! size. Coordinates are pixels, as in the scene: one per column, two per row. Land is solid
//! from its top down, and its tops sit on cell boundaries so it stays crisp in a terminal.

use super::Station;
use super::cast;

/// The most blocks one pile on the bench holds.
pub(super) const PILE: u8 = 3;
/// Columns between the centres of neighbouring exits: room for the widest body and its frame.
const EXIT_PITCH: i32 = 7;
/// How far above the plateau a second row of exits floats.
const EXIT_ROW: i32 = 10;

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
            Spot::Start | Spot::Porch | Spot::Exit => None,
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

#[derive(Debug, Clone)]
pub(super) struct Level {
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
}

impl Level {
    /// `cols` × `rows` are cells. Row 0 is the strips; the land leaves
    /// sky above it for the title, a line of narration, a name and the tallest character.
    /// Small terminals get flatter levels and lose the name tags.
    pub fn new(plan: &Plan, cols: u16, rows: u16) -> Level {
        let (w, rows) = (i32::from(cols), i32::from(rows));
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
        let at = |f: f32| (w as f32 * f).round() as i32;
        let mut top = vec![ground; w.max(0) as usize];
        let mut fill = |from: i32, to: i32, y: i32| {
            for x in from.max(0)..to.min(w) {
                top[x as usize] = y;
            }
        };

        let (a, b) = (at(0.04) + 1, at(0.22));
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
        let (a, b) = (at(0.25), at(0.45));
        let pitch = ((b - a) / plan.piles.len() as i32).max(2);
        let block = plan.block.min(pitch - 1);
        let slots = (0..plan.piles.len() as i32).map(|i| a + i * pitch + (pitch - block) / 2).collect();

        let gap_at = at(0.58);
        let gap = (plan.gap + (w - 80) / 20).clamp(3, 14);
        fill(at(0.47), gap_at, lift(plan.ledge));
        fill(gap_at, gap_at + gap, floor);
        let landing = gap_at + gap;
        fill(landing, w, lift(plan.landing));
        let first_step = landing + 3 + w / 40;
        let tread = plan.tread + w / 80;
        for j in 1..=plan.steps {
            let x = first_step + (j - 1) * tread;
            fill(x, x + tread, lift(plan.landing + j));
        }
        let high = (first_step + plan.steps * tread, plan.high + w / 40);
        fill(high.0, high.0 + high.1, lift(plan.landing + plan.steps + 1));

        Level {
            cols: w,
            floor,
            ground,
            top,
            names: w >= 70 && rows >= 18,
            shelves,
            slots,
            block,
            lip: gap_at - 1,
            landing,
            high,
            exits: (high.0 + high.1, lift(plan.landing) - 1),
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

impl Leg {
    /// How long it takes, in arbitrary units: a jump costs more than a step.
    pub fn cost(self) -> f32 {
        match self {
            Leg::Walk { from, to } => (to - from).abs() as f32,
            Leg::Jump { from, to, .. } => 5.0 + (to.0 - from.0).abs() as f32 * 0.6,
        }
    }
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
        for l in [&a, &b] {
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
