//! Reaping under pressure: with more sessions running than `reap.live`, or free memory below
//! `reap.free`, idle sessions are asked to reap themselves. A working session is never
//! chosen; among idle ones the least recently active goes first (LRU). A chosen session
//! checks for itself that it is still idle.

use std::fs;

use reaper_core::events::ReapReason;
use reaper_core::rules::Config;

use super::store::{Live, Store};

/// Which of `live` to reap, and why. Past the cap: the longest idle, as many as it takes.
/// Short of memory: the longest idle one, unless a reap under way will free some already;
/// how much one frees isn't known, so the next look takes another if it's still short.
pub fn choose(live: &[Live], cap: Option<usize>, short_of_memory: bool) -> Vec<(usize, ReapReason)> {
    let mut idle: Vec<usize> = (0..live.len()).filter(|&i| !live[i].marked && live[i].idle_since.is_some()).collect();
    idle.sort_by_key(|&i| live[i].idle_since);
    let staying = live.iter().filter(|l| !l.marked).count();
    let over = cap.map_or(0, |cap| staying.saturating_sub(cap));
    let mut chosen: Vec<(usize, ReapReason)> = idle.iter().take(over).map(|&i| (i, ReapReason::Crowded)).collect();
    if short_of_memory && chosen.is_empty() && !live.iter().any(|l| l.marked) {
        chosen.extend(idle.first().map(|&i| (i, ReapReason::Memory)));
    }
    chosen
}

fn available() -> Option<u64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let kb = info.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?.split_whitespace().next()?;
    kb.parse::<u64>().ok()?.checked_mul(1024)
}

/// `crowding` when the number of sessions may have changed (one started, or one went idle
/// and became a candidate); a tool call only looks at memory. Without pressure this costs
/// one read of /proc/meminfo at most.
pub fn relieve(config: &Config, store: &Store, crowding: bool) {
    let cap = config.reap_live().filter(|_| crowding);
    let short = config.reap_free().is_some_and(|floor| available().is_some_and(|free| free < floor));
    if cap.is_none() && !short {
        return;
    }
    let live = store.live();
    for (i, reason) in choose(&live, cap, short) {
        store.mark(&live[i], reason.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(states: &[(Option<i64>, bool)]) -> Vec<Live> {
        states
            .iter()
            .enumerate()
            .map(|(i, &(idle_since, marked))| Live { file: format!("/live/{i}.json").into(), idle_since, marked })
            .collect()
    }

    #[test]
    fn past_the_cap_the_longest_idle_go_and_working_sessions_never() {
        // Working, idle since 30, idle since 10, idle since 20.
        let sessions = live(&[(None, false), (Some(30), false), (Some(10), false), (Some(20), false)]);
        assert_eq!(choose(&sessions, Some(2), false), [(2, ReapReason::Crowded), (3, ReapReason::Crowded)]);
        assert_eq!(choose(&sessions, Some(4), false), []);
        // Everyone working: nobody to reap, however crowded.
        assert_eq!(choose(&live(&[(None, false), (None, false)]), Some(1), false), []);
    }

    #[test]
    fn sessions_already_asked_count_as_gone() {
        let sessions = live(&[(Some(10), true), (Some(20), false), (Some(30), false)]);
        assert_eq!(choose(&sessions, Some(1), false), [(1, ReapReason::Crowded)]);
        assert_eq!(choose(&sessions, Some(2), false), []);
    }

    #[test]
    fn short_of_memory_one_goes_at_a_time() {
        let sessions = live(&[(Some(20), false), (Some(10), false)]);
        assert_eq!(choose(&sessions, None, true), [(1, ReapReason::Memory)]);
        let pending = live(&[(Some(20), false), (Some(10), true)]);
        assert_eq!(choose(&pending, None, true), [], "wait for the reap under way");
        // Crowding already frees some.
        assert_eq!(choose(&sessions, Some(1), true), [(1, ReapReason::Crowded)]);
    }
}
