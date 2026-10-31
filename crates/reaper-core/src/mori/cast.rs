//! Who an agent is in memento mori: a name, a colour, a shape and a personality, all from a
//! stable hash of its id, so an agent is the same character in every replay.

/// FNV-1a over the parts, finished with MurmurHash3's mixer so any slice of the bits can
/// choose something. Stable across runs and platforms, unlike std's randomised hasher.
pub fn hash(parts: &[&[u8]]) -> u64 {
    const PRIME: u64 = 0x0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &byte in *part {
            h = (h ^ u64::from(byte)).wrapping_mul(PRIME);
        }
        h = (h ^ 0xff).wrapping_mul(PRIME);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

/// For choices that differ from one event to the next.
pub fn seed(id: &str, n: u64) -> u64 {
    hash(&[id.as_bytes(), &n.to_le_bytes()])
}

/// Short, warm, easy to tell apart. None comes from any game.
pub const NAMES: [&str; 40] = [
    "Ines", "Otto", "Marlow", "Pell", "Juniper", "Sorrel", "Tamsin", "Wren", "Bram", "Cato", "Esme", "Fenn", "Hollis",
    "Ivo", "Jory", "Kit", "Lark", "Merit", "Noor", "Odile", "Piet", "Quill", "Rafe", "Saffi", "Teo", "Ulla", "Vesper",
    "Wilder", "Yara", "Zev", "Alder", "Briar", "Cass", "Dov", "Elio", "Faye", "Gil", "Hale", "Isko", "Juno",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn mix(self, to: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgb(m(self.0, to.0), m(self.1, to.1), m(self.2, to.2))
    }

    /// Darker, keeping `f` of the brightness.
    pub fn dim(self, f: f32) -> Rgb {
        self.mix(Rgb(0, 0, 0), 1.0 - f)
    }
}

/// Soft, distinct hues that read on dark and light terminals. Agents only ever get these.
pub const HUES: [Rgb; 8] = [
    Rgb(0xE8, 0xA3, 0x3D), // amber
    Rgb(0x3F, 0xB8, 0xAF), // teal
    Rgb(0x9B, 0x7F, 0xD4), // violet
    Rgb(0x5A, 0xA9, 0xE6), // sky
    Rgb(0x7B, 0xC6, 0x7E), // leaf
    Rgb(0xD9, 0xC2, 0x7A), // sand
    Rgb(0x8F, 0xE3, 0xC8), // mint
    Rgb(0x5D, 0x6B, 0xD8), // indigo
];

/// The palette on 256-colour terminals: chosen to keep the hues apart, which isn't always
/// the nearest match (violet, sand and mint sit between two).
const HUES_256: [u8; 8] = [179, 73, 140, 74, 114, 186, 121, 62];

pub fn ansi256(rgb: Rgb) -> Option<u8> {
    match rgb {
        REAPER => Some(167),
        DEAD => Some(59),
        _ => HUES.iter().position(|&h| h == rgb).map(|i| HUES_256[i]),
    }
}

/// reaper's own colour: the contact flash, strike pips, the grim reaper's eyes. Never an agent's.
pub const REAPER: Rgb = Rgb(0xE5, 0x48, 0x4D);
/// The dead, and only the dead.
pub const DEAD: Rgb = Rgb(0x5B, 0x5F, 0x66);
/// A flash of light, and paper.
pub const WHITE: Rgb = Rgb(0xF2, 0xF0, 0xE8);

/// The sky behind every level, which memento mori paints itself so it looks the same on any
/// terminal: dusk, from the top of memento mori down to the horizon.
pub const DUSK: (Rgb, Rgb) = (Rgb(0x14, 0x18, 0x30), Rgb(0x48, 0x36, 0x5A));
/// Solid ground: darker than the sky and every character.
pub const LAND: Rgb = Rgb(0x0D, 0x0C, 0x14);
/// The loose blocks on the bench: ground that can be moved.
pub const BLOCK: Rgb = Rgb(0x2A, 0x26, 0x3E);

/// Size in pixels: a terminal cell is one pixel wide and two tall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub w: i32,
    pub h: i32,
}

impl Shape {
    /// Larger models stand taller, small fast ones sit short and wide, unknown ones are square.
    pub fn for_model(model: Option<&str>) -> Shape {
        let Some(model) = model.map(str::to_lowercase).filter(|m| !m.is_empty()) else {
            return Shape { w: 3, h: 3 };
        };
        // Whole words, so "gemini" isn't "mini".
        let words: Vec<&str> = model.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let has = |names: &[&str]| words.iter().any(|w| names.contains(w));
        let local = model.contains(':')
            || words
                .iter()
                .any(|w| ["llama", "qwen", "gemma", "phi", "mistral", "local"].iter().any(|l| w.starts_with(l)));
        if local || has(&["haiku", "mini", "flash", "nano", "small"]) {
            Shape { w: 4, h: 2 }
        } else if has(&["opus", "pro", "max"]) || model.starts_with("gpt-5") {
            Shape { w: 3, h: 6 }
        } else {
            Shape { w: 3, h: 4 }
        }
    }

    pub fn half(self) -> Shape {
        Shape { w: (self.w + 1) / 2, h: (self.h + 1) / 2 }
    }

    pub fn flat(self) -> Shape {
        Shape { w: self.w.max(self.h), h: self.w.min(self.h) }
    }
}

/// How a character moves and how the narrator voices it. Never what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Personality {
    Dreamer,
    Grump,
    ShowOff,
    Worrier,
    Hero,
    Loner,
}

pub const PERSONALITIES: [Personality; 6] = [
    Personality::Dreamer,
    Personality::Grump,
    Personality::ShowOff,
    Personality::Worrier,
    Personality::Hero,
    Personality::Loner,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable() {
        // Pinned: a change here recasts every agent in every replay.
        assert_eq!(hash(&[b"demo-feature-7f3a2c"]), 0x6699_c493_0766_5c96);
        assert_ne!(seed("a", 1), seed("a", 2));
        assert_ne!(hash(&[b"ab", b"c"]), hash(&[b"a", b"bc"]), "parts are separated");
    }

    #[test]
    fn shapes_follow_the_model() {
        let shape = |m: &str| Shape::for_model(Some(m));
        assert_eq!(Shape::for_model(None), Shape { w: 3, h: 3 });
        assert_eq!(shape("claude-opus-5"), Shape { w: 3, h: 6 });
        assert_eq!(shape("gpt-5"), Shape { w: 3, h: 6 });
        assert_eq!(shape("gemini-2.5-pro"), Shape { w: 3, h: 6 }, "gemini is not mini");
        assert_eq!(shape("gpt-5-mini"), Shape { w: 4, h: 2 }, "small wins over large");
        assert_eq!(shape("claude-haiku-4-5"), Shape { w: 4, h: 2 });
        assert_eq!(shape("qwen3:4b"), Shape { w: 4, h: 2 });
        assert_eq!(shape("claude-sonnet-5"), Shape { w: 3, h: 4 });
        assert_eq!(shape("claude-opus-5").half(), Shape { w: 2, h: 3 });
        assert_eq!(Shape { w: 3, h: 6 }.flat(), Shape { w: 6, h: 3 });
    }

    #[test]
    fn mixing_reaches_both_ends() {
        assert_eq!(HUES[0].mix(DEAD, 0.0), HUES[0]);
        assert_eq!(HUES[0].mix(DEAD, 1.0), DEAD);
        assert_eq!(WHITE.dim(0.0), Rgb(0, 0, 0));
    }
}
