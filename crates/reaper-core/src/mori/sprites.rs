//! Pixel art for the death scenes. One character per pixel, `.` is transparent, and letters
//! index the palette in [`ink`]. Drawn with half blocks, a 14-pixel sprite takes 7 rows.

use super::cast::{REAPER, Rgb};

#[derive(Debug)]
pub struct Sprite {
    pub rows: &'static [&'static str],
}

impl Sprite {
    pub fn w(&self) -> i32 {
        self.rows.first().map_or(0, |r| r.len() as i32)
    }

    pub fn h(&self) -> i32 {
        self.rows.len() as i32
    }

    /// Every opaque pixel as (x, y, colour), from the top left.
    pub fn pixels(&self) -> impl Iterator<Item = (i32, i32, Rgb)> + '_ {
        self.rows.iter().enumerate().flat_map(|(y, row)| {
            row.bytes().enumerate().filter_map(move |(x, key)| ink(key).map(|c| (x as i32, y as i32, c)))
        })
    }
}

pub fn ink(key: u8) -> Option<Rgb> {
    Some(match key {
        b'K' => Rgb(0x1F, 0x1A, 0x2C), // robe: a silhouette against the dusk
        b'k' => Rgb(0x3A, 0x33, 0x52), // robe fold
        b'W' => Rgb(0xE8, 0xE2, 0xD0), // bone
        b'r' => REAPER,                // the eyes: the only red on any sprite
        b'S' => Rgb(0xB8, 0xC0, 0xCC), // scythe blade
        b'H' => Rgb(0x7A, 0x5A, 0x3A), // scythe handle
        b'F' => Rgb(0x9A, 0x85, 0x72), // wolf fur
        b'f' => Rgb(0x6B, 0x5A, 0x4C), // wolf ears, tail tip
        b'y' => Rgb(0xF5, 0xD7, 0x6E), // wolf eye
        b'T' => Rgb(0xA0, 0x78, 0x4C), // guillotine timber
        b'B' => Rgb(0xDD, 0xE3, 0xEA), // guillotine blade
        b'b' => Rgb(0xC8, 0xA8, 0x78), // basket
        b'D' => Rgb(0xC9, 0xC9, 0xC9), // dust
        b'P' => Rgb(0xF2, 0x9B, 0xC4), // the bunny's ears
        b'E' => Rgb(0xF4, 0xF1, 0xEA), // bunny fur
        b'e' => Rgb(0x2A, 0x24, 0x30), // bunny eye
        b'g' => Rgb(0xE8, 0xC2, 0x4E), // the holy hand grenade
        b'G' => Rgb(0xA8, 0x82, 0x2C), // its bands
        b'U' => Rgb(0xA9, 0xB8, 0xD0), // the saucer
        b'L' => Rgb(0xCF, 0xE6, 0xEE), // its dome
        _ => return None,
    })
}

/// Faces left, toward the agent.
pub static GRIM: Sprite = Sprite {
    rows: &[
        "..SSSSSSH",
        ".S..kK..H",
        "S..kKKK.H",
        "..kKKKKKH",
        "..KrWrK.H",
        "..KWWWK.H",
        ".kKKKKKKH",
        ".KKKKKKWH",
        "kKKKKKKKH",
        "KKKKKKKKH",
        "KKKKKKKKH",
        "KKKKKKKKH",
        "KKKKKKKKH",
        ".K.K.K.KH",
    ],
};

/// Faces left; two leg frames for running.
pub static WOLF: [Sprite; 2] = [
    Sprite { rows: &[".ff.......", "fFFf......", "yFFFFFFFf.", ".FFFFFFFFf", ".F.F...F.F"] },
    Sprite { rows: &[".ff.......", "fFFf......", "yFFFFFFFf.", ".FFFFFFFFf", "..F.F.F.F."] },
];

/// The guillotine's frame; its blade is separate so it can drop.
pub static FRAME: Sprite = Sprite {
    rows: &[
        "TTTTTTTTT",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "T.......T",
        "TT.....TT",
    ],
};

/// Rests two pixels below the top of the frame, one pixel in from its left post.
pub static BLADE: Sprite = Sprite { rows: &["BBBBB", "BBBB."] };

pub static BASKET: Sprite = Sprite { rows: &["b...b", "bbbbb", ".bbb."] };

/// Sitting, facing left. Sweet.
pub static BUNNY: Sprite = Sprite { rows: &[".E.E..", ".P.P..", "EEEE..", "eEEEEE", ".EEEEE"] };

/// The holy hand grenade. Its pin, a little cross on top, comes out.
pub static GRENADE: Sprite = Sprite { rows: &[".gGg.", "gWGgg", "GGGGG", "ggggg", ".ggg."] };
pub static PIN: Sprite = Sprite { rows: &[".g.", "ggg", ".g."] };

pub static SAUCER: Sprite = Sprite { rows: &["...LLL...", ".UUUUUUU.", "UyUyUyUyU"] };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprites_are_rectangular_and_only_the_reaper_has_red() {
        let all = [&GRIM, &WOLF[0], &WOLF[1], &FRAME, &BLADE, &BASKET, &BUNNY, &GRENADE, &PIN, &SAUCER];
        for sprite in all {
            assert!(sprite.rows.iter().all(|r| r.len() as i32 == sprite.w()), "{sprite:?}");
            for row in sprite.rows {
                assert!(row.bytes().all(|k| k == b'.' || ink(k).is_some()), "unknown key in {row}");
            }
        }
        let red = |s: &Sprite| s.pixels().filter(|p| p.2 == REAPER).count();
        assert_eq!(red(&GRIM), 2, "two eyes");
        assert_eq!(all.iter().skip(1).map(|s| red(s)).sum::<usize>(), 0);
        assert_eq!((GRIM.w(), GRIM.h(), FRAME.h()), (9, 14, 16));
    }
}
