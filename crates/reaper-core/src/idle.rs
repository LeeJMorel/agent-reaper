//! `reap.idle: auto`: how long to keep an idle session, learned from how long its human
//! actually takes to come back: the timeout with the least expected cost over those waits
//! (the ski-rental trade-off). A return that finds its session still running saves a
//! resume, every idle minute holds memory, and a resume counts as `RESUME_WORTH` idle
//! minutes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::events::{Event, Kind, parse_line};
use crate::rules::{Config, DEFAULT_IDLE, duration_ms};

/// One resume costs as much as holding an idle session for this long, in ms: the exchange
/// rate between a person's interruption and memory held.
const RESUME_WORTH: u64 = 10 * 60_000;
/// Waits looked at: the most recent ones, as habits change.
const RECENT: usize = 300;
/// Fewer returns than this say too little; the default applies until there are enough.
const ENOUGH: usize = 20;
/// The learned timeout stays within these, in ms.
const SHORTEST: u64 = 60_000;
const LONGEST: u64 = 30 * 60_000;
const TAIL_BYTES: u64 = 2 << 20;

/// A session waiting on its human, in ms: until they came back, or so far, for a wait that
/// hasn't ended (as good as never, once it's longer than any timeout considered).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wait {
    pub ms: u64,
    pub returned: bool,
}

/// The waits in a log, oldest first: from going idle to the next sign of the human (a new
/// prompt's first action, a resume). Being reaped isn't one. Waits still open at the end
/// count once they're longer than any timeout would be.
pub fn waits(events: &[Event]) -> Vec<Wait> {
    let mut idle_since: HashMap<&str, i64> = HashMap::new();
    let mut waits = Vec::new();
    for event in events {
        match event.kind {
            Kind::Reaped { .. } | Kind::Unknown => {}
            _ => {
                if let Some(since) = idle_since.remove(event.agent.as_str()) {
                    waits.push(Wait { ms: event.at.saturating_sub(since).max(0) as u64, returned: true });
                }
                if event.kind == Kind::Idle {
                    idle_since.insert(&event.agent, event.at);
                }
            }
        }
    }
    let end = events.iter().map(|e| e.at).max().unwrap_or(0);
    let mut open: Vec<i64> = idle_since.into_values().collect();
    open.sort_unstable();
    let open = open.into_iter().map(|since| end.saturating_sub(since).max(0) as u64);
    waits.extend(open.filter(|&ms| ms > LONGEST).map(|ms| Wait { ms, returned: false }));
    waits
}

pub fn learned(waits: &[Wait]) -> Option<u64> {
    let recent = &waits[waits.len().saturating_sub(RECENT)..];
    if recent.iter().filter(|w| w.returned).count() < ENOUGH {
        return None;
    }
    let cost = |timeout: u64| -> u64 {
        recent.iter().map(|w| w.ms.min(timeout) + if w.returned && w.ms > timeout { RESUME_WORTH } else { 0 }).sum()
    };
    let candidates = recent.iter().filter(|w| w.returned).map(|w| w.ms.clamp(SHORTEST, LONGEST));
    // Ties go to the shorter timeout.
    candidates.chain([SHORTEST, LONGEST]).min_by_key(|&t| (cost(t), t))
}

/// The first line, likely cut, is skipped.
pub fn recent_events(log: &Path) -> Vec<Event> {
    let Ok(mut file) = File::open(log) else { return Vec::new() };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(TAIL_BYTES);
    let mut text = String::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let lines = text.lines().skip(usize::from(start > 0));
    lines.filter_map(parse_line).collect()
}

/// `None` when reaping is off.
pub fn reap_after_ms(config: &Config, log: &Path) -> Option<u64> {
    if config.idle() != "auto" {
        return config.reap_after_ms();
    }
    learned(&waits(&recent_events(log))).or_else(|| duration_ms(DEFAULT_IDLE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(at: i64, agent: &str, kind: &str) -> Event {
        let extra = if kind == "agent.reaped" { r#","idle":1"# } else { "" };
        parse_line(&format!(r#"{{"at":{at},"host":"pi","agent":"{agent}","type":"{kind}","tool":"bash"{extra}}}"#))
            .unwrap()
    }

    fn back(ms: u64) -> Wait {
        Wait { ms, returned: true }
    }

    #[test]
    fn a_return_is_the_human_coming_back_not_reaper_reaping() {
        let log = [
            event(0, "a", "agent.idle"),
            event(10, "b", "agent.idle"),
            event(300, "a", "agent.reaped"),
            event(500, "a", "agent.acted"),
            event(510, "a", "agent.acted"),
            event(600, "a", "agent.idle"),
            event(700, "b", "agent.acted"),
            event(800, "c", "agent.idle"),
            event(9_999_000, "e", "agent.idle"),
            event(10_000_000, "d", "agent.acted"),
        ];
        // a and c have waited longer than any timeout would be; e's wait is too young to say.
        let never = |ms| Wait { ms, returned: false };
        assert_eq!(waits(&log), [back(500), back(690), never(9_999_400), never(9_999_200)]);
    }

    #[test]
    fn the_timeout_lands_after_the_quick_returns_and_before_the_long_breaks() {
        assert_eq!(learned(&[back(120_000); ENOUGH - 1]), None, "too little to go on");
        // Back within 4 minutes, or after an hour: worth keeping for the first, not the second.
        let mut waits: Vec<Wait> = (1..=30).map(|i| back(i * 8_000)).collect();
        waits.extend([back(3_600_000); 10]);
        assert_eq!(learned(&waits), Some(240_000));
        // Sessions nobody comes back to pull it down too.
        let mut abandoned = waits.clone();
        abandoned.extend([Wait { ms: 7_200_000, returned: false }; 40]);
        assert!(learned(&abandoned).unwrap() <= 240_000);
        assert_eq!(learned(&[back(1_000); 40]), Some(SHORTEST), "never shorter than a minute");
        assert_eq!(learned(&[back(86_400_000); 40]), Some(SHORTEST), "keeping helps nobody back after a day");
        assert_eq!(learned(&[back(5 * 60_000); 40]), Some(5 * 60_000), "worth keeping for");
        assert_eq!(learned(&[back(25 * 60_000); 40]), Some(SHORTEST), "a resume costs less than 25 idle minutes");
    }
}
