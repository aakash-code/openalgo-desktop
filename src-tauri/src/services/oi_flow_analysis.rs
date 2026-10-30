//! Writer Flow analysis: what option writers did at each strike, step by step.
//!
//! Pure: no database, no broker, no clock, no logging. The recorded samples
//! come in as an argument and every reading leaves as the return value, the
//! same rule `services/risk/` follows in the web, so the page, a /trading panel
//! and an alert can share one implementation and the tests need nothing
//! running. Faithful port of the fork's `services/oi_flow_analysis.py`.
//!
//! Reading a step. Open interest alone cannot tell a writer from a buyer, so a
//! strike side (call or put) is classed by its OI and its premium together:
//!
//! ```text
//!   OI up,   premium down  -> writing    (writers adding: short build-up)
//!   OI down, premium up    -> covering   (writers exiting: short covering)
//!   OI up,   premium up    -> buying     (long build-up)
//!   OI down, premium down  -> unwinding  (long unwinding)
//!   OI up or down, premium unchanged -> adding / reducing (side unclear)
//!   a change below the noise floor   -> steady
//! ```
//!
//! A streak is a run of one OI direction, and its class is read from the
//! premium across the whole run rather than from each step, because exchange OI
//! arrives in jumps and a per-step class flickers. A single quiet step does not
//! end a run.

use chrono::NaiveDateTime;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const WRITING: &str = "writing";
pub const COVERING: &str = "covering";
pub const BUYING: &str = "buying";
pub const UNWINDING: &str = "unwinding";
pub const ADDING: &str = "adding";
pub const REDUCING: &str = "reducing";
pub const STEADY: &str = "steady";

/// The two classes that say what writers are doing; events are raised for these.
fn is_writer_class(cls: &str) -> bool {
    cls == WRITING || cls == COVERING
}

/// Premiums sit on their contract's tick grid, so two prices are either equal
/// or at least one tick apart. This only absorbs floating-point noise.
const PRICE_EPSILON: f64 = 1e-6;

/// One recorded sample, as `oi_flow_db` stores it.
#[derive(Clone, Debug)]
pub struct Sample {
    pub sample_time: NaiveDateTime,
    pub strike: f64,
    /// "CE" or "PE".
    pub side: String,
    pub symbol: String,
    pub oi: Option<f64>,
    pub ltp: Option<f64>,
    pub spot: Option<f64>,
    pub lot_size: Option<i64>,
}

/// How the day is read. Every field is a setting the trader can change.
#[derive(Clone, Debug)]
pub struct FlowSettings {
    /// Seconds per step; None chooses 60 or 180 from how often OI changed today.
    pub step_seconds: Option<i64>,
    /// A change smaller than both of these is noise: a percent of what the
    /// strike side held before the step, and a number of lots.
    pub noise_pct: f64,
    pub noise_lots: i64,
    /// Minutes without a meaningful move that end a streak.
    pub quiet_minutes_to_break: f64,
    /// Streak lengths, in minutes, at which an event is raised.
    pub event_minutes: Vec<i64>,
    /// Windows, in minutes, the net change is reported over.
    pub windows: Vec<i64>,
}

impl Default for FlowSettings {
    fn default() -> Self {
        Self {
            step_seconds: None,
            noise_pct: 0.5,
            noise_lots: 5,
            quiet_minutes_to_break: 5.0,
            event_minutes: vec![10, 15, 20],
            windows: vec![3, 10, 15, 20],
        }
    }
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|f| f.is_finite())
}

/// The open interest in a sample, or None when it is not a reading. A 0 after
/// the contract has held some is a dropped quote, not every writer leaving at
/// once; a 0 before it ever held is real (nobody has traded it yet).
pub fn reading_oi(value: Option<f64>, held_before: bool) -> Option<i64> {
    let f = finite(value)?;
    if f < 0.0 || (f == 0.0 && held_before) {
        return None;
    }
    Some(f as i64)
}

/// The premium in a sample, or None when it is not a price (missing, 0, not
/// finite). 0 is a contract that has not traded yet, not a price.
pub fn reading_ltp(value: Option<f64>) -> Option<f64> {
    finite(value).filter(|f| *f > 0.0)
}

/// What each reading means for the underlying. Call writers covering and put
/// writers adding are the two halves of a rally.
fn bias_of(side: &str, cls: &str) -> i32 {
    // Call writers covering and put writers adding are the two halves of a
    // rally; their opposites are the two halves of a fall. (Literals match the
    // class consts above.)
    let bullish = matches!(
        (side, cls),
        ("CE", "covering") | ("CE", "buying") | ("PE", "writing") | ("PE", "unwinding")
    );
    let bearish = matches!(
        (side, cls),
        ("CE", "writing") | ("CE", "unwinding") | ("PE", "covering") | ("PE", "buying")
    );
    if bullish {
        1
    } else if bearish {
        -1
    } else {
        0
    }
}

/// +1 bullish, -1 bearish, 0 neither. Public mirror of the web's `bias`.
pub fn bias(side: &str, cls: &str) -> i32 {
    bias_of(side, cls)
}

/// The class of an OI move in one direction, given the premium over it.
fn direction_class(up: bool, ltp_change: f64) -> &'static str {
    if ltp_change > PRICE_EPSILON {
        if up {
            BUYING
        } else {
            COVERING
        }
    } else if ltp_change < -PRICE_EPSILON {
        if up {
            WRITING
        } else {
            UNWINDING
        }
    } else if up {
        ADDING
    } else {
        REDUCING
    }
}

/// One step's class for one strike side.
fn classify(
    oi_change: i64,
    ltp_change: f64,
    prev_oi: i64,
    lot_size: i64,
    settings: &FlowSettings,
) -> &'static str {
    let floor = (settings.noise_pct / 100.0 * prev_oi.max(0) as f64)
        .max((settings.noise_lots * lot_size.max(1)) as f64);
    if (oi_change.abs() as f64) < floor || oi_change == 0 {
        return STEADY;
    }
    direction_class(oi_change > 0, ltp_change)
}

/// The start of the step a timestamp belongs to: truncated to `step` seconds
/// from the day's midnight (session-anchored bucketing of the recorded minute).
fn step_start(ts: NaiveDateTime, step: i64) -> NaiveDateTime {
    let midnight = ts.date().and_hms_opt(0, 0, 0).unwrap();
    let seconds = (ts - midnight).num_seconds();
    midnight + chrono::Duration::seconds(seconds - seconds % step)
}

/// 60 when OI changes about every minute, 180 when it arrives in slower jumps:
/// the median gap between successive OI changes, across every contract.
fn choose_step_seconds(samples: &[Sample]) -> i64 {
    let mut ordered: Vec<&Sample> = samples.iter().collect();
    ordered.sort_by_key(|s| s.sample_time);

    let mut by_symbol: HashMap<&str, Vec<(NaiveDateTime, i64)>> = HashMap::new();
    for s in ordered {
        let series = by_symbol.entry(s.symbol.as_str()).or_default();
        let held_before = series.last().map(|(_, v)| *v > 0).unwrap_or(false);
        if let Some(oi) = reading_oi(s.oi, held_before) {
            series.push((s.sample_time, oi));
        }
    }

    let mut gaps: Vec<f64> = Vec::new();
    for series in by_symbol.values() {
        let changed: Vec<NaiveDateTime> = (1..series.len())
            .filter(|&i| series[i].1 != series[i - 1].1)
            .map(|i| series[i].0)
            .collect();
        for w in changed.windows(2) {
            gaps.push((w[1] - w[0]).num_seconds() as f64);
        }
    }
    if gaps.is_empty() {
        return 180;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = gaps.len() / 2;
    let median = if gaps.len() % 2 == 0 {
        (gaps[mid - 1] + gaps[mid]) / 2.0
    } else {
        gaps[mid]
    };
    if median <= 90.0 {
        60
    } else {
        180
    }
}

/// A run of one OI direction. Its class is read from the premium across the
/// whole run, not from any one step.
struct Run {
    up: bool,
    start_index: usize,
    start_ltp: Option<f64>,
    last_ltp: Option<f64>,
    oi_change: i64,
    quiet_minutes: f64,
    raised: BTreeSet<i64>,
}

impl Run {
    fn cls(&self) -> &'static str {
        match (self.start_ltp, self.last_ltp) {
            (Some(start), Some(last)) => direction_class(self.up, last - start),
            _ => {
                if self.up {
                    ADDING
                } else {
                    REDUCING
                }
            }
        }
    }
}

#[derive(Serialize)]
pub struct Cell {
    pub time: NaiveDateTime,
    pub strike: f64,
    pub side: String,
    pub cls: String,
    pub oi_change: i64,
    pub ltp_change: f64,
    pub oi: i64,
}

#[derive(Serialize)]
pub struct Total {
    pub time: NaiveDateTime,
    pub spot: Option<f64>,
    pub ce_change: i64,
    pub pe_change: i64,
}

#[derive(Serialize)]
pub struct SideState {
    pub cls: String,
    pub streak_minutes: f64,
    pub streak_oi_change: i64,
    pub steady_minutes: f64,
    pub oi: i64,
    pub window_change: BTreeMap<i64, Option<i64>>,
    pub observed: bool,
    pub last_seen: Option<NaiveDateTime>,
}

#[derive(Serialize)]
pub struct StrikeState {
    pub strike: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ce: Option<SideState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pe: Option<SideState>,
    pub observed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atm: Option<bool>,
}

#[derive(Serialize)]
pub struct Event {
    pub time: NaiveDateTime,
    pub strike: f64,
    pub side: String,
    pub kind: String,
    pub minutes: i64,
    pub oi_change: i64,
}

#[derive(Serialize)]
pub struct FlowAnalysis {
    pub step_seconds: i64,
    pub oi_unit: String,
    pub steps: Vec<NaiveDateTime>,
    pub spot: Option<f64>,
    pub cells: Vec<Cell>,
    pub totals: Vec<Total>,
    pub strikes: Vec<StrikeState>,
    pub events: Vec<Event>,
}

fn empty(step_seconds: i64) -> FlowAnalysis {
    FlowAnalysis {
        step_seconds,
        oi_unit: "quantity".to_string(),
        steps: Vec::new(),
        spot: None,
        cells: Vec::new(),
        totals: Vec::new(),
        strikes: Vec::new(),
        events: Vec::new(),
    }
}

/// A usable sample within a step, after the OI/LTP readings are resolved.
#[derive(Clone)]
struct Clean {
    sample_time: NaiveDateTime,
    oi: i64,
    ltp: Option<f64>,
    lot_size: Option<i64>,
}

/// Read a day of samples for one underlying and expiry.
pub fn analyse(samples: &[Sample], settings: &FlowSettings) -> FlowAnalysis {
    if samples.is_empty() {
        return empty(settings.step_seconds.unwrap_or(180));
    }
    let step = settings
        .step_seconds
        .unwrap_or_else(|| choose_step_seconds(samples));
    let step_minutes = step as f64 / 60.0;
    let max_gap_steps =
        (2.0_f64).max((settings.quiet_minutes_to_break / step_minutes).ceil()) as i64;

    // The last usable sample of each step, per contract.
    let mut ordered: Vec<&Sample> = samples.iter().collect();
    ordered.sort_by_key(|s| s.sample_time);

    // key = (strike-bits, side); using ordered-float bits keeps a BTreeMap key.
    type Key = (u64, String);
    let mut last: BTreeMap<Key, BTreeMap<NaiveDateTime, Clean>> = BTreeMap::new();
    let mut held: BTreeSet<Key> = BTreeSet::new();
    let mut spot_at: BTreeMap<NaiveDateTime, f64> = BTreeMap::new();

    for s in &ordered {
        let key: Key = (s.strike.to_bits(), s.side.clone());
        let oi = reading_oi(s.oi, held.contains(&key));
        let t = step_start(s.sample_time, step);
        if let Some(spot) = finite(s.spot).filter(|v| *v > 0.0) {
            spot_at.insert(t, spot);
        }
        let Some(oi) = oi else { continue };
        if oi > 0 {
            held.insert(key.clone());
        }
        last.entry(key).or_default().insert(
            t,
            Clean {
                sample_time: s.sample_time,
                oi,
                ltp: reading_ltp(s.ltp),
                lot_size: s.lot_size,
            },
        );
    }

    let steps: Vec<NaiveDateTime> = last
        .values()
        .flat_map(|series| series.keys().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let index: HashMap<NaiveDateTime, usize> =
        steps.iter().enumerate().map(|(i, t)| (*t, i)).collect();

    let mut cells: Vec<Cell> = Vec::new();
    let mut totals: BTreeMap<NaiveDateTime, (i64, i64)> =
        steps.iter().map(|t| (*t, (0, 0))).collect();
    // strike-bits -> (strike, ce, pe)
    let mut strikes: BTreeMap<u64, (f64, Option<SideState>, Option<SideState>)> = BTreeMap::new();
    let mut events: Vec<Event> = Vec::new();

    let mut in_lots = 0i64;
    let mut in_quantity = 0i64;

    for ((strike_bits, side), series) in &last {
        let strike = f64::from_bits(*strike_bits);
        let lot = oi_lot(series);
        let first_lot = series
            .values()
            .next()
            .and_then(|c| c.lot_size)
            .unwrap_or(1);
        if lot == 1 && first_lot > 1 {
            in_lots += 1;
        } else {
            in_quantity += 1;
        }

        let mut prev: Option<&Clean> = None;
        let mut prev_index: i64 = -1;
        let mut run: Option<Run> = None;
        let mut last_move_index: Option<i64> = None;
        let mut oi_at: BTreeMap<i64, i64> = BTreeMap::new();
        let mut last_seen: Option<NaiveDateTime> = None;

        for t in &steps {
            let Some(s) = series.get(t) else { continue };
            let i = index[t] as i64;
            let oi = s.oi;
            let ltp = s.ltp;
            oi_at.insert(i, oi);
            last_seen = Some(s.sample_time);

            if prev.is_none() || i - prev_index > max_gap_steps {
                prev = Some(s);
                prev_index = i;
                run = None;
                continue;
            }
            let p = prev.unwrap();
            let prev_oi = p.oi;
            let prev_ltp = p.ltp;
            let d_oi = oi - prev_oi;
            let d_ltp = match (ltp, prev_ltp) {
                (Some(a), Some(b)) => a - b,
                _ => 0.0,
            };
            let cls = classify(d_oi, d_ltp, prev_oi, lot, settings);
            cells.push(Cell {
                time: *t,
                strike,
                side: side.clone(),
                cls: cls.to_string(),
                oi_change: d_oi,
                ltp_change: (d_ltp * 100.0).round() / 100.0,
                oi,
            });
            let entry = totals.get_mut(t).unwrap();
            if side == "CE" {
                entry.0 += d_oi;
            } else {
                entry.1 += d_oi;
            }

            if cls == STEADY {
                if let Some(r) = run.as_mut() {
                    r.quiet_minutes += step_minutes;
                    r.oi_change += d_oi;
                    if ltp.is_some() {
                        r.last_ltp = ltp;
                    }
                    if r.quiet_minutes >= settings.quiet_minutes_to_break {
                        run = None;
                    }
                }
            } else {
                last_move_index = Some(i);
                let up = d_oi > 0;
                match run.as_mut() {
                    Some(r) if r.up == up => {
                        r.quiet_minutes = 0.0;
                        r.oi_change += d_oi;
                        if ltp.is_some() {
                            r.last_ltp = ltp;
                        }
                    }
                    _ => {
                        run = Some(Run {
                            up,
                            start_index: i as usize,
                            start_ltp: prev_ltp,
                            last_ltp: ltp,
                            oi_change: d_oi,
                            quiet_minutes: 0.0,
                            raised: BTreeSet::new(),
                        });
                    }
                }
                let r = run.as_mut().unwrap();
                if is_writer_class(r.cls()) {
                    let minutes = (i - r.start_index as i64 + 1) as f64 * step_minutes;
                    let marks: Vec<i64> = settings.event_minutes.clone();
                    for mark in marks {
                        if minutes >= mark as f64 && !r.raised.contains(&mark) {
                            r.raised.insert(mark);
                            events.push(Event {
                                time: *t,
                                strike,
                                side: side.clone(),
                                kind: r.cls().to_string(),
                                minutes: mark,
                                oi_change: r.oi_change,
                            });
                        }
                    }
                }
            }
            prev = Some(s);
            prev_index = i;
        }

        let end = steps.len() as i64 - 1;
        let (state_cls, streak_minutes, streak_change) = match run.as_ref() {
            Some(r) => (
                r.cls(),
                (end - r.start_index as i64 + 1) as f64 * step_minutes,
                r.oi_change,
            ),
            None => (STEADY, 0.0, 0),
        };
        let steady_minutes = match last_move_index {
            Some(m) => (end - m) as f64 * step_minutes,
            None => end as f64 * step_minutes,
        };
        let latest_i = oi_at.keys().last().copied();
        let latest_oi = latest_i.map(|i| oi_at[&i]).unwrap_or(0);
        let mut window_change: BTreeMap<i64, Option<i64>> = BTreeMap::new();
        for w in &settings.windows {
            let back = (*w as f64 / step_minutes).round() as i64;
            let earlier = oi_at.keys().filter(|k| **k <= end - back).max().copied();
            window_change.insert(*w, earlier.map(|k| latest_oi - oi_at[&k]));
        }
        let observed = latest_i == Some(end);
        let state = SideState {
            cls: state_cls.to_string(),
            streak_minutes,
            streak_oi_change: streak_change,
            steady_minutes: if state_cls == STEADY { steady_minutes } else { 0.0 },
            oi: latest_oi,
            window_change,
            observed,
            last_seen,
        };
        let row = strikes.entry(*strike_bits).or_insert((strike, None, None));
        if side == "CE" {
            row.1 = Some(state);
        } else {
            row.2 = Some(state);
        }
    }

    // The latest price known at the last step.
    let spot = steps.last().and_then(|last_step| {
        spot_at
            .range(..=*last_step)
            .next_back()
            .map(|(_, v)| *v)
    });

    let mut rows: Vec<StrikeState> = strikes
        .into_values()
        .map(|(strike, ce, pe)| {
            let observed =
                ce.as_ref().map(|s| s.observed).unwrap_or(false) || pe.as_ref().map(|s| s.observed).unwrap_or(false);
            StrikeState {
                strike,
                ce,
                pe,
                observed,
                atm: None,
            }
        })
        .collect();

    if let Some(spot) = spot {
        if let Some(atm) = rows
            .iter()
            .min_by(|a, b| {
                (a.strike - spot)
                    .abs()
                    .partial_cmp(&(b.strike - spot).abs())
                    .unwrap()
            })
            .map(|r| r.strike)
        {
            for r in rows.iter_mut() {
                r.atm = Some(r.strike == atm);
            }
        }
    }

    events.sort_by(|a, b| {
        (a.time, a.strike.to_bits(), a.side.clone()).cmp(&(b.time, b.strike.to_bits(), b.side.clone()))
    });

    FlowAnalysis {
        step_seconds: step,
        oi_unit: if in_lots > in_quantity {
            "lots".to_string()
        } else {
            "quantity".to_string()
        },
        steps: steps.clone(),
        spot,
        cells,
        totals: steps
            .iter()
            .map(|t| {
                let (ce, pe) = totals[t];
                Total {
                    time: *t,
                    spot: spot_at.get(t).copied(),
                    ce_change: ce,
                    pe_change: pe,
                }
            })
            .collect(),
        strikes: rows,
        events,
    }
}

/// What one lot is in this contract's open interest numbers, over the resolved
/// series: lot size, or 1. Read from the data rather than a table of exchanges
/// -- NFO/BFO quote quantity (every reading a whole number of lots), MCX lots.
fn oi_lot(series: &BTreeMap<NaiveDateTime, Clean>) -> i64 {
    let lot = series
        .values()
        .next()
        .and_then(|c| c.lot_size)
        .unwrap_or(1)
        .max(1);
    if lot == 1 {
        return 1;
    }
    let held: Vec<i64> = series.values().map(|c| c.oi).filter(|v| *v != 0).collect();
    if held.is_empty() {
        return lot;
    }
    if held.iter().all(|v| v % lot == 0) {
        lot
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 16)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap()
            + chrono::Duration::minutes(min as i64)
    }

    fn sample(min: u32, strike: f64, side: &str, oi: f64, ltp: f64, spot: f64) -> Sample {
        Sample {
            sample_time: t(min),
            strike,
            side: side.to_string(),
            symbol: format!("NIFTY{}{}", strike as i64, side),
            oi: Some(oi),
            ltp: Some(ltp),
            spot: Some(spot),
            lot_size: Some(65),
        }
    }

    #[test]
    fn classifies_a_step_by_oi_and_premium() {
        let s = FlowSettings::default();
        // OI up a lot, premium down -> writing.
        assert_eq!(classify(100_000, -1.5, 1_000_000, 65, &s), WRITING);
        // OI down, premium up -> covering.
        assert_eq!(classify(-100_000, 1.5, 1_000_000, 65, &s), COVERING);
        // Below the noise floor -> steady.
        assert_eq!(classify(10, 1.0, 1_000_000, 65, &s), STEADY);
    }

    #[test]
    fn bias_reads_the_two_halves_of_a_rally() {
        assert_eq!(bias("CE", COVERING), 1);
        assert_eq!(bias("PE", WRITING), 1);
        assert_eq!(bias("CE", WRITING), -1);
        assert_eq!(bias("PE", COVERING), -1);
        assert_eq!(bias("CE", STEADY), 0);
    }

    #[test]
    fn a_writing_streak_raises_an_event_and_marks_atm() {
        let s = FlowSettings {
            step_seconds: Some(60),
            ..Default::default()
        };
        // 23000 CE writing minute by minute for 12 minutes: OI climbs, premium falls.
        let mut samples = Vec::new();
        for m in 0..=12 {
            let oi = 1_000_000.0 + (m as f64) * 200_000.0;
            let ltp = 120.0 - (m as f64) * 2.0;
            samples.push(sample(m, 23000.0, "CE", oi, ltp, 22950.0));
            // a quiet put so there are two strikes and a spot
            samples.push(sample(m, 22950.0, "PE", 500_000.0, 80.0, 22950.0));
        }
        let out = analyse(&samples, &s);
        assert_eq!(out.step_seconds, 60);
        // A 10-minute writing event on the 23000 CE.
        assert!(out
            .events
            .iter()
            .any(|e| e.strike == 23000.0 && e.side == "CE" && e.kind == WRITING && e.minutes == 10));
        // ATM is the strike nearest spot (22950).
        assert!(out
            .strikes
            .iter()
            .any(|r| r.strike == 22950.0 && r.atm == Some(true)));
    }
}
