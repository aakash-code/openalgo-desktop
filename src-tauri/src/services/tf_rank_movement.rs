//! TF Boost rank-movement: the PURE core, ported from the fork's
//! `services/tf_rank_movement_service.py`. Given a symbol's `[minute, rank]`
//! observations it folds the current-day movement state -- delta, velocity,
//! acceleration, and trailing improving/deteriorating runs. No I/O (same
//! discipline as `services/risk/`). The DuckDB snapshot recorder and the
//! fuller top-N / sustained / run / classify passes are later stages.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RankState {
    pub symbol: String,
    pub observations: usize,
    pub current_rank: i64,
    /// None until a second observation: the first is a baseline, not a move.
    pub previous_rank: Option<i64>,
    pub first_seen_rank: i64,
    pub best_rank: i64,
    pub worst_rank: i64,
    pub first_seen_min: f64,
    pub last_seen_min: f64,
    pub previous_seen_min: Option<f64>,
    /// previous_rank - current_rank; positive = improved (rank fell).
    pub rank_delta: Option<i64>,
    pub rank_change_since_first_seen: i64,
    /// Ranks improved per minute over the last step.
    pub rank_velocity: Option<f64>,
    /// Last step's velocity minus the prior step's.
    pub rank_acceleration: Option<f64>,
    pub consecutive_improving: i64,
    pub consecutive_deteriorating: i64,
}

/// Strictly-increasing `(minute, rank)` pairs; a pair whose minute is not after
/// the last kept one is skipped (dupes/disorder), so no zero-elapsed step can
/// divide the velocity by zero. Minute is a float -- the recorder samples twice
/// a minute (604.5 = 10:04:30).
fn clean_observations<T: Copy>(observations: &[(f64, T)]) -> Vec<(f64, T)> {
    let mut clean: Vec<(f64, T)> = Vec::new();
    for &(m, r) in observations {
        if let Some(&(last_m, _)) = clean.last() {
            if m <= last_m {
                continue;
            }
        }
        clean.push((m, r));
    }
    clean
}

/// Pure: fold one symbol's `[minute, rank]` pairs into a [`RankState`], or None
/// when nothing is usable.
pub fn compute_rank_state(symbol: &str, observations: &[(f64, i64)]) -> Option<RankState> {
    let clean = clean_observations(observations);
    if clean.is_empty() {
        return None;
    }
    let (first_min, first_rank) = clean[0];
    let (last_min, current_rank) = clean[clean.len() - 1];
    let best_rank = clean.iter().map(|&(_, r)| r).min().unwrap();
    let worst_rank = clean.iter().map(|&(_, r)| r).max().unwrap();

    let mut previous_rank = None;
    let mut previous_min = None;
    let mut rank_delta = None;
    let mut rank_velocity = None;
    let mut rank_acceleration = None;
    let mut prev_velocity: Option<f64> = None;
    let mut consec_improve = 0i64;
    let mut consec_deteriorate = 0i64;

    // Each step uses the ACTUAL elapsed minutes, so a gap widens the
    // denominator rather than being filled in.
    for w in clean.windows(2) {
        let (m0, r0) = w[0];
        let (m1, r1) = w[1];
        let dt = m1 - m0; // > 0 by construction
        let velocity = (r0 - r1) as f64 / dt;
        if let Some(pv) = prev_velocity {
            rank_acceleration = Some(velocity - pv);
        }
        prev_velocity = Some(velocity);
        if r1 < r0 {
            consec_improve += 1;
            consec_deteriorate = 0;
        } else if r1 > r0 {
            consec_deteriorate += 1;
            consec_improve = 0;
        } else {
            consec_improve = 0;
            consec_deteriorate = 0;
        }
        previous_rank = Some(r0);
        previous_min = Some(m0);
        rank_delta = Some(r0 - r1);
        rank_velocity = Some(velocity);
    }

    Some(RankState {
        symbol: symbol.to_string(),
        observations: clean.len(),
        current_rank,
        previous_rank,
        first_seen_rank: first_rank,
        best_rank,
        worst_rank,
        first_seen_min: first_min,
        last_seen_min: last_min,
        previous_seen_min: previous_min,
        rank_delta,
        rank_change_since_first_seen: first_rank - current_rank,
        rank_velocity,
        rank_acceleration,
        consecutive_improving: consec_improve,
        consecutive_deteriorating: consec_deteriorate,
    })
}

/// Top-N zones the engine tracks (rank <= threshold), best (smallest) first.
pub const TOP_N_THRESHOLDS: [i64; 3] = [5, 10, 20];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TopNZone {
    pub threshold: i64,
    pub inside: bool,
    pub entered_min: Option<f64>,
    pub entries: i64,
    /// Span of the open stretch since the last entry (0 when outside).
    pub continuous_min: f64,
    /// Every stretch spent inside today, summed.
    pub cumulative_min: f64,
    pub last_event: Option<String>,
    pub last_event_min: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TopNTransition {
    pub minute: f64,
    pub threshold: i64,
    /// "ENTRY" (first time today) | "RE_ENTRY" (a later entry) | "EXIT".
    pub kind: String,
}

/// Pure: per-zone standing plus the ordered threshold crossings. A crossing
/// fires only on the boundary (so an hour inside Top-10 is one ENTRY, not
/// thirty); a symbol that simply stops appearing is left inside with its
/// last-known standing, never forced to EXIT (an absent snapshot is not a rank).
pub fn compute_topn(
    observations: &[(f64, i64)],
    thresholds: &[i64],
) -> (BTreeMap<i64, TopNZone>, Vec<TopNTransition>) {
    let clean = clean_observations(observations);
    let mut zones: BTreeMap<i64, TopNZone> = BTreeMap::new();
    let mut transitions: Vec<TopNTransition> = Vec::new();

    for &n in thresholds {
        let mut inside = false;
        let mut entered_min: Option<f64> = None;
        let mut last_inside_min: Option<f64> = None;
        let mut entries = 0i64;
        let mut cumulative = 0.0f64;
        let mut last_event: Option<String> = None;
        let mut last_event_min: Option<f64> = None;

        for &(m, r) in &clean {
            let now_inside = r <= n;
            if now_inside && !inside {
                entries += 1;
                entered_min = Some(m);
                last_inside_min = Some(m);
                let kind = if entries == 1 { "ENTRY" } else { "RE_ENTRY" };
                transitions.push(TopNTransition { minute: m, threshold: n, kind: kind.to_string() });
                last_event = Some(format!("TOP{n}_{kind}"));
                last_event_min = Some(m);
            } else if now_inside && inside {
                last_inside_min = Some(m);
            } else if !now_inside && inside {
                // Close the stretch at the last minute actually seen inside.
                if let (Some(e), Some(li)) = (entered_min, last_inside_min) {
                    cumulative += li - e;
                }
                transitions.push(TopNTransition { minute: m, threshold: n, kind: "EXIT".to_string() });
                last_event = Some(format!("TOP{n}_EXIT"));
                last_event_min = Some(m);
                entered_min = None;
            }
            inside = now_inside;
        }

        let mut continuous = 0.0;
        if inside {
            if let (Some(e), Some(li)) = (entered_min, last_inside_min) {
                continuous = li - e;
                cumulative += continuous;
            }
        }

        zones.insert(
            n,
            TopNZone {
                threshold: n,
                inside,
                entered_min,
                entries,
                continuous_min: continuous,
                cumulative_min: cumulative,
                last_event,
                last_event_min,
            },
        );
    }
    (zones, transitions)
}

// ----------------------------------------------------------------- sustained

/// Default rolling window and max spread for [`compute_sustained`] (API
/// defaults, like [`TOP_N_THRESHOLDS`]).
pub const SUSTAINED_WINDOW: usize = 6;
const SUSTAINED_MIN_OBS: usize = 3;
pub const STABLE_ZONE_MAX_SPREAD: i64 = 5;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SustainedState {
    pub window: usize,
    pub zone_low: i64,
    pub zone_high: i64,
    pub zone_spread: i64,
    pub zone_median: f64,
    pub is_stable_zone: bool,
    pub sustained_top5: bool,
    pub sustained_top10: bool,
    pub sustained_top20: bool,
}

fn median(values: &[i64]) -> f64 {
    let mut s = values.to_vec();
    s.sort_unstable();
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2] as f64
    } else {
        (s[n / 2 - 1] + s[n / 2]) as f64 / 2.0
    }
}

/// Pure: fold the last `window` observations into a stability view. "Sustained"
/// means SETTLED (enough evidence AND a tight spread), so a symbol still
/// climbing is inside Top-10 the whole way yet not established there.
pub fn compute_sustained(
    observations: &[(f64, i64)],
    window: usize,
    max_spread: i64,
    thresholds: &[i64],
) -> Option<SustainedState> {
    let clean = clean_observations(observations);
    if clean.is_empty() {
        return None;
    }
    let start = clean.len().saturating_sub(window);
    let win: Vec<i64> = clean[start..].iter().map(|&(_, r)| r).collect();
    let low = *win.iter().min().unwrap();
    let high = *win.iter().max().unwrap();
    let spread = high - low;
    let stable = win.len() >= SUSTAINED_MIN_OBS && spread <= max_spread;
    Some(SustainedState {
        window: win.len(),
        zone_low: low,
        zone_high: high,
        zone_spread: spread,
        zone_median: median(&win),
        is_stable_zone: stable,
        sustained_top5: stable && high <= thresholds[0],
        sustained_top10: stable && high <= thresholds[1],
        sustained_top20: stable && high <= thresholds[2],
    })
}

// ------------------------------------------------------------ directional run

const RUN_MIN_OBS: usize = 10;
const RUN_MIN_MOVE_PCT: f64 = 0.6;
const RUN_MIN_EFFICIENCY: f64 = 3.0;
const RUN_ADVERSE_FLOOR: f64 = 0.15;
const RUN_SUSTAIN_MIN: f64 = 5.0;
const RUN_FLIP_FACTOR: f64 = 1.5;
/// 09:20, as minutes-of-day: the first prints are pre-open figures, not prices.
const RUN_SETTLE_MIN: f64 = 9.0 * 60.0 + 20.0;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunState {
    pub observations: usize,
    pub direction: Option<String>,
    pub move_pct: f64,
    pub adverse_pct: f64,
    pub efficiency: f64,
    pub from_min: f64,
    pub run_minutes: f64,
    pub is_clean: bool,
    pub sustained: bool,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Direction carried forward, reversing only on decisive evidence. Returns
/// (direction, low_index, high_index) -- the final global extremes.
fn walk_direction(values: &[f64], flip_factor: f64) -> (Option<&'static str>, usize, usize) {
    let mut direction: Option<&'static str> = None;
    let (mut low_i, mut high_i) = (0usize, 0usize);
    for (i, &v) in values.iter().enumerate() {
        if v < values[low_i] {
            low_i = i;
        }
        if v > values[high_i] {
            high_i = i;
        }
        let up = v - values[low_i];
        let down = v - values[high_i];
        if up == 0.0 && down == 0.0 {
            continue;
        }
        match direction {
            None => direction = Some(if up.abs() >= down.abs() { "up" } else { "down" }),
            Some("up") => {
                if down.abs() > up.abs() * flip_factor {
                    direction = Some("down");
                }
            }
            _ => {
                if up.abs() > down.abs() * flip_factor {
                    direction = Some("up");
                }
            }
        }
    }
    (direction, low_i, high_i)
}

/// Pure: fold `[minute, change_pct]` into the current move's shape -- move
/// against give-back, anchored where the current direction began, not at the
/// open. `sustain_min` holds is_clean up briefly after it stops passing, so one
/// run does not strobe into many badges.
pub fn compute_run(changes: &[(f64, f64)]) -> Option<RunState> {
    let clean: Vec<(f64, f64)> = clean_observations(changes)
        .into_iter()
        .filter(|&(m, _)| m >= RUN_SETTLE_MIN)
        .collect();
    if clean.len() < RUN_MIN_OBS {
        return None;
    }
    let values: Vec<f64> = clean.iter().map(|&(_, v)| v).collect();
    let now = *values.last().unwrap();

    let (direction, low_i, high_i) = walk_direction(&values, RUN_FLIP_FACTOR);

    let (anchor, mv) = match direction {
        Some("up") => (low_i, now - values[low_i]),
        Some("down") => (high_i, now - values[high_i]),
        _ => (values.len() - 1, 0.0),
    };

    let mut adverse = 0.0f64;
    let mut last_clean_min: Option<f64> = None;
    if direction.is_some() {
        let base = values[anchor];
        let mut extreme = values[anchor];
        for idx in anchor..values.len() {
            let value = values[idx];
            extreme = if direction == Some("up") {
                extreme.max(value)
            } else {
                extreme.min(value)
            };
            let give_back = if direction == Some("up") {
                extreme - value
            } else {
                value - extreme
            };
            adverse = adverse.max(give_back);
            let step_move = value - base;
            let step_eff = step_move.abs() / adverse.max(RUN_ADVERSE_FLOOR);
            if step_move.abs() >= RUN_MIN_MOVE_PCT && step_eff >= RUN_MIN_EFFICIENCY {
                last_clean_min = Some(clean[idx].0);
            }
        }
    }

    let efficiency = mv.abs() / adverse.max(RUN_ADVERSE_FLOOR);
    let clean_now = direction.is_some() && mv.abs() >= RUN_MIN_MOVE_PCT && efficiency >= RUN_MIN_EFFICIENCY;
    let sustained = !clean_now
        && last_clean_min.is_some_and(|lc| clean.last().unwrap().0 - lc <= RUN_SUSTAIN_MIN);

    Some(RunState {
        observations: clean.len(),
        direction: direction.map(str::to_string),
        move_pct: round2(mv),
        adverse_pct: round2(adverse),
        efficiency: round2(efficiency),
        from_min: clean[anchor].0,
        run_minutes: clean.last().unwrap().0 - clean[anchor].0,
        is_clean: clean_now || sustained,
        sustained,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_observations_give_delta_and_velocity() {
        let s = compute_rank_state("A", &[(0.0, 62), (4.0, 20)]).unwrap();
        assert_eq!(s.observations, 2);
        assert_eq!(s.current_rank, 20);
        assert_eq!(s.previous_rank, Some(62));
        assert_eq!(s.rank_delta, Some(42));
        assert_eq!(s.rank_velocity, Some(10.5)); // (62-20)/4
        assert_eq!(s.rank_change_since_first_seen, 42);
        assert_eq!(s.best_rank, 20);
        assert_eq!(s.worst_rank, 62);
        assert_eq!(s.consecutive_improving, 1);
        assert_eq!(s.rank_acceleration, None); // only one step
    }

    #[test]
    fn a_single_observation_is_a_baseline_not_a_move() {
        let s = compute_rank_state("NEW", &[(100.0, 15)]).unwrap();
        assert_eq!(s.observations, 1);
        assert_eq!(s.current_rank, 15);
        assert_eq!(s.previous_rank, None);
        assert_eq!(s.rank_delta, None);
        assert_eq!(s.rank_velocity, None);
        assert_eq!(s.rank_change_since_first_seen, 0);
    }

    #[test]
    fn a_duplicate_minute_is_skipped() {
        // The (0.0, 50) pair repeats minute 0 and is dropped, so this equals
        // the two-observation case above.
        let s = compute_rank_state("A", &[(0.0, 62), (0.0, 50), (4.0, 20)]).unwrap();
        assert_eq!(s.observations, 2);
        assert_eq!(s.rank_velocity, Some(10.5));
    }

    #[test]
    fn acceleration_is_the_change_in_velocity() {
        // Steps: (0->2) v=3.5, (2->4) v=4.0 -> accel = 4.0 - 3.5 = 0.5.
        let s = compute_rank_state("C", &[(0.0, 62), (2.0, 55), (4.0, 47)]).unwrap();
        assert_eq!(s.rank_velocity, Some(4.0));
        assert_eq!(s.rank_acceleration, Some(0.5));
        assert_eq!(s.consecutive_improving, 2);
    }

    #[test]
    fn empty_is_none() {
        assert!(compute_rank_state("X", &[]).is_none());
    }

    #[test]
    fn topn_tracks_entry_exit_reentry() {
        // rank: 12 (out), 8 (enter T10), 6 (in), 15 (exit T10), 9 (re-enter T10).
        let obs = [(0.0, 12), (2.0, 8), (4.0, 6), (6.0, 15), (8.0, 9)];
        let (zones, transitions) = compute_topn(&obs, &TOP_N_THRESHOLDS);

        let t10 = &zones[&10];
        assert!(t10.inside);
        assert_eq!(t10.entries, 2);
        assert_eq!(t10.cumulative_min, 2.0); // the 2->4 stretch; the re-entry is 0 wide so far
        assert_eq!(t10.last_event.as_deref(), Some("TOP10_RE_ENTRY"));

        let kinds: Vec<&str> = transitions
            .iter()
            .filter(|t| t.threshold == 10)
            .map(|t| t.kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["ENTRY", "EXIT", "RE_ENTRY"]);

        // Never reached Top-5 (best rank 6); always within Top-20 (worst 15).
        assert_eq!(zones[&5].entries, 0);
        assert!(!zones[&5].inside);
        assert_eq!(zones[&20].entries, 1);
        assert!(zones[&20].inside);
    }

    #[test]
    fn sustained_needs_evidence_and_a_tight_spread() {
        // A settled 6-8 zone over 6 observations: stable, sustained in Top-10/20.
        let obs = [(0.0, 6), (1.0, 7), (2.0, 6), (3.0, 8), (4.0, 7), (5.0, 6)];
        let s = compute_sustained(&obs, SUSTAINED_WINDOW, STABLE_ZONE_MAX_SPREAD, &TOP_N_THRESHOLDS)
            .unwrap();
        assert_eq!(s.zone_low, 6);
        assert_eq!(s.zone_high, 8);
        assert_eq!(s.zone_spread, 2);
        assert!(s.is_stable_zone);
        assert!(!s.sustained_top5); // 8 > 5
        assert!(s.sustained_top10);
        assert!(s.sustained_top20);

        // A chop (spread 15) is never a zone.
        let chop = [(0.0, 9), (1.0, 2), (2.0, 17)];
        let c = compute_sustained(&chop, SUSTAINED_WINDOW, STABLE_ZONE_MAX_SPREAD, &TOP_N_THRESHOLDS)
            .unwrap();
        assert!(!c.is_stable_zone);
        assert!(!c.sustained_top20);
    }

    #[test]
    fn a_clean_up_run_is_measured_from_the_turn() {
        // 11 post-settle observations rising 0.0 -> 2.0 with no give-back.
        let obs: Vec<(f64, f64)> = (0..=10).map(|i| (560.0 + i as f64, 0.2 * i as f64)).collect();
        let r = compute_run(&obs).unwrap();
        assert_eq!(r.direction.as_deref(), Some("up"));
        assert_eq!(r.observations, 11);
        assert_eq!(r.move_pct, 2.0);
        assert_eq!(r.adverse_pct, 0.0);
        assert!(r.is_clean);
    }

    #[test]
    fn a_run_before_the_settle_minute_is_dropped() {
        // All pre-09:20 -> filtered out -> fewer than RUN_MIN_OBS -> None.
        let obs: Vec<(f64, f64)> = (0..=10).map(|i| (540.0 + i as f64, 0.2 * i as f64)).collect();
        assert!(compute_run(&obs).is_none());
    }
}
