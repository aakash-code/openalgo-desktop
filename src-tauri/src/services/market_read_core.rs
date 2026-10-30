//! Market Read core -- the levels subset, ported from the fork's
//! `services/market_read/core.py`. Pure (no I/O): the recorded samples come in
//! and the readings leave as return values, so Option Levels and (later) the
//! Market Read panel share one implementation.
//!
//! This file carries only what Option Levels needs: the minute `Book`, `near`,
//! the support/resistance `level_events`, and the shared event/format helpers
//! and constants. The flow / futures / volatility readings and `build_read`
//! are a later addition for the Market Read panel. (Volume is read as 0 here;
//! the flow reading that needs it will thread it through then.)
//!
//! NOT yet verified against a running feed -- compiles and the pure helpers are
//! unit-tested, but the stateful event logic needs a live Desktop to confirm.

use crate::services::oi_flow_analysis::Sample;
use chrono::NaiveDateTime;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const LAKH: f64 = 100_000.0;
pub const HOLD_MINUTES: i64 = 3;
pub const BAND_STEPS: i64 = 6;
pub const LEVEL_STEPS: i64 = 10;
pub const SIGNIFICANT_SHARE: f64 = 0.5;
pub const WEAKEN_SHARE: f64 = 0.10;
pub const WEAKEN_RESET_SHARE: f64 = 0.02;
pub const LEVEL_FLIP_COOLDOWN: i64 = 30;

pub const MAJOR: &str = "major";
pub const INFO: &str = "info";

/// A key into a minute's legs: option side ("CE"/"PE") and strike (by bits, so
/// it is Ord/Hash). Use [`strike_of`] to read the strike back.
type LegKey = (&'static str, u64);

fn side_static(side: &str) -> &'static str {
    if side == "CE" {
        "CE"
    } else {
        "PE"
    }
}

fn strike_of(key: &LegKey) -> f64 {
    f64::from_bits(key.1)
}

/// (oi, ltp, volume) for one leg at one minute.
type Leg = (f64, f64, f64);

#[derive(Clone)]
pub struct Book {
    pub times: Vec<NaiveDateTime>,
    pub legs: BTreeMap<NaiveDateTime, HashMap<LegKey, Leg>>,
    pub spot: BTreeMap<NaiveDateTime, f64>,
    pub step: f64,
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    if n == 0 {
        0.0
    } else if n % 2 == 0 {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    } else {
        xs[n / 2]
    }
}

/// The most common gap between adjacent strikes (the strike step). Ties take
/// the smallest, matching Python's `statistics.mode` on the first-seen order
/// closely enough for a uniform strike grid.
fn mode_gap(gaps: &[f64]) -> Option<f64> {
    if gaps.is_empty() {
        return None;
    }
    let mut counts: BTreeMap<u64, usize> = BTreeMap::new();
    for g in gaps {
        *counts.entry(g.to_bits()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(_, c)| c)
        .map(|(bits, _)| f64::from_bits(bits))
}

/// The minute book: legs keyed by (side, strike) per minute, the median spot,
/// and the strike step. None when fewer than two minutes carry both OI and spot.
pub fn book(samples: &[Sample]) -> Option<Book> {
    let mut legs: BTreeMap<NaiveDateTime, HashMap<LegKey, Leg>> = BTreeMap::new();
    let mut spots: BTreeMap<NaiveDateTime, Vec<f64>> = BTreeMap::new();
    for r in samples {
        let Some(oi) = r.oi else { continue };
        let key: LegKey = (side_static(&r.side), r.strike.to_bits());
        legs.entry(r.sample_time)
            .or_default()
            .insert(key, (oi, r.ltp.unwrap_or(0.0), 0.0));
        if let Some(s) = r.spot {
            if s > 0.0 {
                spots.entry(r.sample_time).or_default().push(s);
            }
        }
    }
    let times: Vec<NaiveDateTime> = legs
        .keys()
        .filter(|t| spots.contains_key(t))
        .copied()
        .collect();
    if times.len() < 2 {
        return None;
    }
    let mut strikes: Vec<f64> = times
        .iter()
        .flat_map(|t| legs[t].keys().map(|k| k.1))
        .collect::<BTreeSet<u64>>()
        .into_iter()
        .map(f64::from_bits)
        .collect();
    strikes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let gaps: Vec<f64> = strikes.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    let step = mode_gap(&gaps)?;
    let book_legs: BTreeMap<_, _> = times.iter().map(|t| (*t, legs[t].clone())).collect();
    let book_spot: BTreeMap<_, _> = times
        .iter()
        .map(|t| (*t, median(spots[t].clone())))
        .collect();
    Some(Book {
        times,
        legs: book_legs,
        spot: book_spot,
        step,
    })
}

/// The strikes within `steps` of spot at minute `t`, sorted.
pub fn near(book: &Book, t: &NaiveDateTime, steps: i64) -> Vec<f64> {
    let spot = book.spot[t];
    let mut out: Vec<f64> = book.legs[t]
        .keys()
        .filter(|k| (strike_of(k) - spot).abs() <= steps as f64 * book.step + 1e-9)
        .map(|k| k.1)
        .collect::<BTreeSet<u64>>()
        .into_iter()
        .map(f64::from_bits)
        .collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap());
    out
}

#[derive(Clone, Serialize)]
pub struct Event {
    pub id: String,
    pub kind: String,
    pub time: NaiveDateTime,
    pub level: String,
    pub title: String,
    pub reason: String,
}

pub fn event(kind: &str, t: NaiveDateTime, level: &str, title: String, reason: String, key: &str) -> Event {
    Event {
        id: format!("{kind}:{key}:{}", t.format("%H%M")),
        kind: kind.to_string(),
        time: t,
        level: level.to_string(),
        title,
        reason,
    }
}

pub fn lakh(x: f64) -> String {
    format!("{:.1} lakh", x / LAKH)
}
pub fn signed_lakh(x: f64) -> String {
    format!("{:+.1} lakh", x / LAKH)
}
pub fn strike_str(k: f64) -> String {
    if k.fract() == 0.0 {
        format!("{k:.0}")
    } else {
        format!("{k:.1}")
    }
}
pub fn hm(t: NaiveDateTime) -> String {
    t.format("%H:%M").to_string()
}

fn oi_of(legs: &HashMap<LegKey, Leg>, side: &'static str, k: f64) -> Option<f64> {
    legs.get(&(side, k.to_bits())).map(|l| l.0)
}

/// Support and resistance from where open interest sits, and their changes:
/// the "levels" reading. Returns the events and the final {support,resistance}
/// anchor strikes. Faithful port of `_level_events`.
pub fn level_events(book: &Book, expiry_day: bool) -> (Vec<Event>, BTreeMap<&'static str, Option<f64>>) {
    let mut events: Vec<Event> = Vec::new();
    let mut peak: HashMap<LegKey, f64> = HashMap::new();
    let mut weakened: BTreeSet<LegKey> = BTreeSet::new();
    // held[side] = (strike, since, minutes)
    let mut held: HashMap<&str, (Option<f64>, Option<NaiveDateTime>, i64)> =
        HashMap::from([("PE", (None, None, 0)), ("CE", (None, None, 0))]);
    let mut bases: HashMap<&str, BTreeMap<u64, NaiveDateTime>> =
        HashMap::from([("PE", BTreeMap::new()), ("CE", BTreeMap::new())]);
    // crossed[(side,strike)] = (first_t, opp_then, minutes)
    let mut crossed: HashMap<LegKey, (NaiveDateTime, f64, i64)> = HashMap::new();
    let mut flipped: BTreeSet<LegKey> = BTreeSet::new();
    let mut last_flip: HashMap<LegKey, NaiveDateTime> = HashMap::new();
    let mut state: BTreeMap<&'static str, Option<f64>> =
        BTreeMap::from([("support", None), ("resistance", None)]);

    for t in &book.times {
        let legs = &book.legs[t];
        let spot = book.spot[t];
        let near_strikes = near(book, t, LEVEL_STEPS);
        for (key, leg) in legs.iter() {
            let e = peak.entry(*key).or_insert(0.0);
            *e = e.max(leg.0);
        }
        for (side, role) in [("PE", "support"), ("CE", "resistance")] {
            let sside = side_static(side);
            let ois: BTreeMap<u64, f64> = near_strikes
                .iter()
                .filter_map(|&k| oi_of(legs, sside, k).map(|oi| (k.to_bits(), oi)))
                .collect();
            let half = book.step / 2.0;
            let own: BTreeMap<u64, f64> = ois
                .iter()
                .filter(|(bits, _)| {
                    let k = f64::from_bits(**bits);
                    if side == "PE" {
                        k <= spot + half
                    } else {
                        k >= spot - half
                    }
                })
                .map(|(b, o)| (*b, *o))
                .collect();
            if own.is_empty() {
                continue;
            }
            let top = f64::from_bits(*own.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0);
            let (mut strike, mut since, mut minutes) = held[side];
            if strike == Some(top) {
                minutes += 1;
            } else {
                strike = Some(top);
                since = Some(*t);
                minutes = 1;
            }
            held.insert(side, (strike, since, minutes));
            if minutes == HOLD_MINUTES {
                let first = bases[side].is_empty();
                bases.get_mut(side).unwrap().entry(top.to_bits()).or_insert(since.unwrap());
                let word = if side == "PE" { "put" } else { "call" };
                let title = if first {
                    format!("{} at {}", cap(role), strike_str(top))
                } else {
                    format!("Biggest {word} base moved to {}", strike_str(top))
                };
                let oi_here = ois[&top.to_bits()];
                events.push(event(
                    &format!("{role}_at"),
                    *t,
                    INFO,
                    title,
                    format!(
                        "{} holds the most {word} open interest near the money: {}.",
                        strike_str(top),
                        lakh(oi_here)
                    ),
                    &strike_str(top),
                ));
            }
            if minutes >= HOLD_MINUTES {
                state.insert(role_static(role), Some(top));
            }

            let biggest = ois.values().cloned().fold(0.0_f64, f64::max);
            for (bits, &oi) in ois.iter() {
                let k = f64::from_bits(*bits);
                let key: LegKey = (sside, *bits);
                let pk = *peak.get(&key).unwrap_or(&0.0);
                if pk < SIGNIFICANT_SHARE * biggest || flipped.contains(&key) {
                    continue;
                }
                if !weakened.contains(&key) && oi <= (1.0 - WEAKEN_SHARE) * pk {
                    weakened.insert(key);
                    let word = if side == "PE" { "Put" } else { "Call" };
                    let close = state.get(role_static(role)) == Some(&Some(k)) && !expiry_day;
                    events.push(event(
                        &format!("{role}_weakening"),
                        *t,
                        if close { MAJOR } else { INFO },
                        format!("{word} writers leaving {}", strike_str(k)),
                        format!(
                            "{word} open interest at {} is {}, {:.0}% below its peak of {}.",
                            strike_str(k),
                            lakh(oi),
                            (1.0 - oi / pk) * 100.0,
                            lakh(pk)
                        ),
                        &strike_str(k),
                    ));
                } else if weakened.contains(&key) && oi >= (1.0 - WEAKEN_RESET_SHARE) * pk {
                    weakened.remove(&key);
                }
            }
        }

        // A level the price has crossed, now defended from the other side.
        for (side, opposite) in [("PE", "CE"), ("CE", "PE")] {
            let sside = side_static(side);
            let oside = side_static(opposite);
            let base_strikes: Vec<u64> = bases[side].keys().copied().collect();
            for kbits in base_strikes {
                let k = f64::from_bits(kbits);
                let key: LegKey = (sside, kbits);
                let beyond = if side == "PE" { spot < k } else { spot > k };
                if !beyond {
                    crossed.remove(&key);
                    flipped.remove(&key);
                    continue;
                }
                let (Some(opp_leg), Some(_own_leg)) =
                    (legs.get(&(oside, kbits)), legs.get(&(sside, kbits)))
                else {
                    continue;
                };
                let (first_t, opp_then, mut minutes) =
                    *crossed.get(&key).unwrap_or(&(*t, opp_leg.0, 0));
                minutes += 1;
                crossed.insert(key, (first_t, opp_then, minutes));
                let opp_now = legs[&(oside, kbits)].0;
                let own_now = legs[&(sside, kbits)].0;
                let pk = *peak.get(&key).unwrap_or(&0.0);
                let cooled = last_flip
                    .get(&key)
                    .map(|f| (*t - *f).num_minutes() >= LEVEL_FLIP_COOLDOWN)
                    .unwrap_or(true);
                if !flipped.contains(&key)
                    && minutes >= HOLD_MINUTES
                    && cooled
                    && opp_now > opp_then
                    && own_now <= (1.0 - WEAKEN_SHARE / 2.0) * pk
                {
                    flipped.insert(key);
                    last_flip.insert(key, *t);
                    let (title, what, kind) = if side == "PE" {
                        (
                            format!("{} turned from support to resistance", strike_str(k)),
                            format!(
                                "Price below {} since {}. Put open interest there is down to {} from {}, and calls written there rose {}.",
                                strike_str(k), hm(first_t), lakh(own_now), lakh(pk), signed_lakh(opp_now - opp_then)
                            ),
                            "support_to_resistance",
                        )
                    } else {
                        (
                            format!("{} turned from resistance to support", strike_str(k)),
                            format!(
                                "Price above {} since {}. Call open interest there is down to {} from {}, and puts written there rose {}.",
                                strike_str(k), hm(first_t), lakh(own_now), lakh(pk), signed_lakh(opp_now - opp_then)
                            ),
                            "resistance_to_support",
                        )
                    };
                    events.push(event(kind, *t, MAJOR, title, what, &strike_str(k)));
                }
            }
        }
    }
    (events, state)
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn role_static(role: &str) -> &'static str {
    if role == "support" {
        "support"
    } else {
        "resistance"
    }
}

// ---------------------------------------------------------------- market read
//
// The flow / levels / price readings of `build_read`, ported from
// market_read/core.py. The futures, India-VIX, implied-volatility and churn
// readings need live quote feeds + option greeks that are not wired yet, so
// they are served as null (the panel renders them blank) -- the OI-driven
// readings (who leads option writing, support/resistance, gaps/extremes) are
// the portable, testable core.

const FLOW_WINDOW: i64 = 10;
const FLOW_SHARE: f64 = 0.01;
const BALANCED_MINUTES: i64 = 10;
const FLIP_SETTLE: i64 = 15;
const GAP_SHARE: f64 = 0.003;
const NEW_EXTREME_PAUSE: i64 = 15;
const NEW_EXTREME_SHARE: f64 = 0.0002;

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
fn iso(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}
fn tone_word(side: Option<&str>) -> i32 {
    match side {
        Some("put") => 1,
        Some("call") => -1,
        _ => 0,
    }
}

struct FlowMinute {
    time: NaiveDateTime,
    ce: f64,
    pe: f64,
    band_oi: f64,
}

/// Call and put open-interest change near the money, minute by minute.
fn flow(book: &Book) -> Vec<FlowMinute> {
    let mut out = Vec::new();
    for w in book.times.windows(2) {
        let (prev, t) = (w[0], w[1]);
        let now = &book.legs[&t];
        let before = &book.legs[&prev];
        let (mut ce, mut pe, mut band) = (0.0, 0.0, 0.0);
        for k in near(book, &t, BAND_STEPS) {
            for side in ["CE", "PE"] {
                let key = (side_static(side), k.to_bits());
                if let Some(leg) = now.get(&key) {
                    band += leg.0;
                    if let Some(was) = before.get(&key) {
                        let change = leg.0 - was.0;
                        if side == "CE" {
                            ce += change;
                        } else {
                            pe += change;
                        }
                    }
                }
            }
        }
        out.push(FlowMinute { time: t, ce, pe, band_oi: band });
    }
    out
}

fn window_sum(v: &[f64], i: usize, n: i64) -> f64 {
    let start = (i as i64 - n + 1).max(0) as usize;
    v[start..=i].iter().sum()
}

struct Regime {
    time: NaiveDateTime,
    side: String,
    since: NaiveDateTime,
}

/// Who leads the flow, held HOLD_MINUTES before it counts. Returns the changes
/// plus the FLOW_WINDOW net flow and its threshold at every minute.
fn regimes(flow: &[FlowMinute]) -> (Vec<Regime>, Vec<f64>, Vec<f64>) {
    let net: Vec<f64> = flow.iter().map(|f| f.pe - f.ce).collect();
    let mut changes: Vec<Regime> = Vec::new();
    let mut current: Option<&str> = None;
    let mut run_side: Option<&str> = None;
    let mut run_start: Option<NaiveDateTime> = None;
    let mut run_len = 0i64;
    let mut quiet = 0i64;
    let mut nets = Vec::new();
    let mut thresholds = Vec::new();
    for (i, f) in flow.iter().enumerate() {
        let n = window_sum(&net, i, FLOW_WINDOW);
        let threshold = FLOW_SHARE * f.band_oi;
        nets.push(n);
        thresholds.push(threshold);
        let side = if n > threshold {
            Some("put")
        } else if n < -threshold {
            Some("call")
        } else {
            None
        };
        match side {
            None => {
                run_side = None;
                run_len = 0;
                quiet += 1;
                if !matches!(current, None | Some("balanced")) && quiet >= BALANCED_MINUTES {
                    current = Some("balanced");
                    changes.push(Regime {
                        time: f.time,
                        side: "balanced".into(),
                        since: flow[i - quiet as usize + 1].time,
                    });
                }
            }
            Some(s) => {
                quiet = 0;
                if run_side == Some(s) {
                    run_len += 1;
                } else {
                    run_side = Some(s);
                    run_start = Some(f.time);
                    run_len = 1;
                }
                if run_len >= HOLD_MINUTES && current != Some(s) {
                    current = Some(s);
                    changes.push(Regime {
                        time: f.time,
                        side: s.into(),
                        since: run_start.unwrap(),
                    });
                }
            }
        }
    }
    (changes, nets, thresholds)
}

fn flow_events(
    u: &str,
    regimes: &[Regime],
    flow: &[FlowMinute],
    index_of: &HashMap<NaiveDateTime, usize>,
) -> Vec<Event> {
    let mut out = Vec::new();
    let mut previous: Option<NaiveDateTime> = None;
    let mut last_side: Option<String> = None;
    for r in regimes {
        let i = index_of[&r.time];
        let start = (i as i64 - FLOW_WINDOW + 1).max(0) as usize;
        let part = &flow[start..=i];
        let ce: f64 = part.iter().map(|f| f.ce).sum();
        let pe: f64 = part.iter().map(|f| f.pe).sum();
        if r.side == "balanced" {
            out.push(event(
                "flow_balanced",
                r.time,
                INFO,
                format!("Neither side leads {u} option writing"),
                format!("No clear lead from puts or calls since {}.", hm(r.since)),
                "",
            ));
            continue;
        }
        let writers = if r.side == "put" { "Put" } else { "Call" };
        let settled = previous
            .map(|p| (r.since - p) >= chrono::Duration::minutes(FLIP_SETTLE))
            .unwrap_or(true);
        let turned = last_side.as_deref() != Some(r.side.as_str());
        previous = Some(r.since);
        last_side = Some(r.side.clone());
        let level = if settled && turned { MAJOR } else { INFO };
        out.push(event(
            &format!("flow_{}", r.side),
            r.time,
            level,
            format!("{writers} writers took over {u}"),
            format!(
                "Since {}, near the money: put open interest {}, call {} in the last {FLOW_WINDOW} minutes.",
                hm(r.since),
                signed_lakh(pe),
                signed_lakh(ce)
            ),
            "",
        ));
    }
    out
}

fn price_events(u: &str, book: &Book, prev_close: Option<f64>) -> Vec<Event> {
    let mut out = Vec::new();
    let times = &book.times;
    let open_ = book.spot[&times[0]];
    if let Some(pc) = prev_close {
        if (open_ - pc).abs() >= GAP_SHARE * pc {
            let up = open_ > pc;
            for t in &times[1..] {
                let s = book.spot[t];
                if (up && s <= pc) || (!up && s >= pc) {
                    out.push(event(
                        "gap_filled",
                        *t,
                        MAJOR,
                        format!("{u} filled its opening gap"),
                        format!(
                            "Opened {:+.0} from the previous close of {pc:.0}, and is now back at {s:.0}.",
                            open_ - pc
                        ),
                        "",
                    ));
                    break;
                }
            }
        }
    }
    let (mut high, mut low) = (open_, open_);
    let (mut high_t, mut low_t) = (times[0], times[0]);
    for t in &times[1..] {
        let s = book.spot[t];
        if s > high {
            if (*t - high_t) >= chrono::Duration::minutes(NEW_EXTREME_PAUSE)
                && s - high >= NEW_EXTREME_SHARE * high
            {
                out.push(event(
                    "day_high",
                    *t,
                    INFO,
                    format!("{u} made a new day high"),
                    format!("{s:.0}, above the {high:.0} it made at {}.", hm(high_t)),
                    "",
                ));
            }
            high = s;
            high_t = *t;
        }
        if s < low {
            if (*t - low_t) >= chrono::Duration::minutes(NEW_EXTREME_PAUSE)
                && low - s >= NEW_EXTREME_SHARE * low
            {
                out.push(event(
                    "day_low",
                    *t,
                    INFO,
                    format!("{u} made a new day low"),
                    format!("{s:.0}, below the {low:.0} it made at {}.", hm(low_t)),
                    "",
                ));
            }
            low = s;
            low_t = *t;
        }
    }
    out
}

/// What one request asks of [`build_read`]. Live market quotes and option
/// greeks are deferred, so the futures/volatility/IV readings stay null.
pub struct ReadInputs {
    pub underlying: String,
    pub samples: Vec<Sample>,
    pub prev_close: Option<f64>,
}

/// The read for the day so far, or None when fewer than two minutes are held.
pub fn build_read(inputs: &ReadInputs) -> Option<Value> {
    let book = book(&inputs.samples)?;
    let u = inputs.underlying.as_str();
    let flow = flow(&book);
    let index_of: HashMap<NaiveDateTime, usize> =
        flow.iter().enumerate().map(|(i, f)| (f.time, i)).collect();
    let (regimes_v, nets, thresholds) = regimes(&flow);
    let (lvl_events, levels) = level_events(&book, false);

    let first = book.times[0];
    let last_t = *book.times.last().unwrap();
    let mut events = flow_events(u, &regimes_v, &flow, &index_of);
    events.extend(lvl_events);
    events.extend(price_events(u, &book, inputs.prev_close));
    events.retain(|e| e.time >= first && e.time <= last_t);
    events.sort_by(|a, b| {
        (a.time, a.level != MAJOR, a.kind.clone()).cmp(&(b.time, b.level != MAJOR, b.kind.clone()))
    });

    let regime = regimes_v.last();
    let net_last = nets.last().copied().unwrap_or(0.0);
    let thr_last = thresholds.last().copied().unwrap_or(0.0);
    let half = thr_last / 2.0;
    let leaning = if net_last > half {
        Some("put")
    } else if net_last < -half {
        Some("call")
    } else {
        None
    };
    let tail = &flow[flow.len().saturating_sub(FLOW_WINDOW as usize)..];
    let flow_state = json!({
        "side": regime.map(|r| r.side.clone()),
        "leaning": leaning,
        "since": regime.map(|r| hm(r.since)),
        "net_lakh": round1(net_last / LAKH),
        "threshold_lakh": round1(thr_last / LAKH),
        "ce_lakh": round1(tail.iter().map(|f| f.ce).sum::<f64>() / LAKH),
        "pe_lakh": round1(tail.iter().map(|f| f.pe).sum::<f64>() / LAKH),
        "delta_net_lakh": Value::Null,
        "tone": regime
            .filter(|r| Some(r.side.as_str()) == leaning)
            .map(|r| tone_word(Some(&r.side)))
            .unwrap_or(0),
    });

    let legs = &book.legs[&last_t];
    let level = |role: &str, side: &str| -> Value {
        let Some(Some(k)) = levels.get(role_static(role)) else { return Value::Null };
        let key = (side_static(side), k.to_bits());
        let Some(leg) = legs.get(&key) else { return Value::Null };
        let oi = leg.0;
        let peak = book
            .times
            .iter()
            .filter_map(|t| book.legs[t].get(&key).map(|l| l.0))
            .fold(0.0_f64, f64::max);
        json!({
            "strike": k,
            "oi_lakh": round1(oi / LAKH),
            "peak_lakh": round1(peak / LAKH),
            "from_peak_pct": if peak > 0.0 { round1((oi / peak - 1.0) * 100.0) } else { 0.0 },
        })
    };

    let spot = book.spot[&last_t];
    let day_high = book.spot.values().cloned().fold(f64::NEG_INFINITY, f64::max);
    let day_low = book.spot.values().cloned().fold(f64::INFINITY, f64::min);
    let state = json!({
        "flow": flow_state,
        "futures": Value::Null,
        "levels": {"support": level("support", "PE"), "resistance": level("resistance", "CE")},
        "volatility": Value::Null,
        "churn": Value::Null,
        "others": {},
        "price": {
            "spot": round2(spot),
            "open": round2(book.spot[&first]),
            "prev_close": inputs.prev_close,
            "day_high": round2(day_high),
            "day_low": round2(day_low),
        },
    });
    let series: Vec<Value> = flow
        .iter()
        .enumerate()
        .map(|(i, f)| {
            json!({
                "time": iso(f.time),
                "spot": round2(book.spot[&f.time]),
                "net_lakh": round1(nets[i] / LAKH),
                "threshold_lakh": round1(thresholds[i] / LAKH),
            })
        })
        .collect();

    Some(json!({
        "underlying": u,
        "minute": iso(last_t),
        "headline": headline(u, &flow_state, &events),
        "state": state,
        "events": events,
        "series": series,
    }))
}

fn headline(u: &str, flow_state: &Value, events: &[Event]) -> String {
    let side = flow_state.get("side").and_then(Value::as_str);
    let leaning = flow_state.get("leaning").and_then(Value::as_str);
    let since = flow_state.get("since").and_then(Value::as_str).unwrap_or("");
    let mut head = match side {
        Some("put") | Some("call") => {
            let who = if side == Some("put") { "Put writers" } else { "Call writers" };
            let mut s = format!("{who} lead {u} since {since}");
            if let Some(l) = leaning {
                if Some(l) != side {
                    s.push_str(&format!(", but {l} writers are taking over"));
                }
            } else {
                s.push_str(", fading");
            }
            s
        }
        _ => format!("Neither puts nor calls lead {u}"),
    };
    head.push('.');
    let major: Vec<&Event> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "support_to_resistance" | "resistance_to_support" | "gap_filled"
            )
        })
        .collect();
    if let Some(last) = major.last() {
        head.push_str(&format!(" {} at {}.", last.title, hm(last.time)));
    }
    head
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(min: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 21)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap()
            + chrono::Duration::minutes(min)
    }
    fn s(min: i64, strike: f64, side: &str, oi: f64, spot: f64) -> Sample {
        Sample {
            sample_time: t(min),
            strike,
            side: side.to_string(),
            symbol: String::new(),
            oi: Some(oi),
            ltp: Some(10.0),
            spot: Some(spot),
            lot_size: Some(65),
        }
    }

    #[test]
    fn book_finds_step_and_near() {
        let mut v = Vec::new();
        for m in 0..3 {
            for strike in [22900.0, 22950.0, 23000.0, 23050.0] {
                v.push(s(m, strike, "CE", 100.0, 22970.0));
                v.push(s(m, strike, "PE", 100.0, 22970.0));
            }
        }
        let b = book(&v).expect("book");
        assert_eq!(b.step, 50.0);
        let n = near(&b, &t(0), 1);
        // within 1 step (50) of 22970: 22950 and 23000.
        assert!(n.contains(&22950.0) && n.contains(&23000.0));
    }

    #[test]
    fn a_held_put_base_raises_a_support_event() {
        let mut v = Vec::new();
        // 22900 PE holds the most put OI near the money for HOLD_MINUTES.
        for m in 0..HOLD_MINUTES {
            v.push(s(m, 22900.0, "PE", 1_000_000.0, 22950.0));
            v.push(s(m, 22950.0, "PE", 400_000.0, 22950.0));
            v.push(s(m, 23000.0, "CE", 500_000.0, 22950.0));
        }
        let b = book(&v).expect("book");
        let (events, state) = level_events(&b, false);
        assert!(events.iter().any(|e| e.kind == "support_at"));
        assert_eq!(state["support"], Some(22900.0));
    }

    #[test]
    fn build_read_reads_put_writers_leading() {
        let mut v = Vec::new();
        // Put open interest climbs near the money while calls stay flat: the
        // flow should read put writers leading. Futures/VIX are deferred (null).
        for m in 0..6 {
            v.push(s(m, 22950.0, "PE", 500_000.0 + (m as f64) * 200_000.0, 22970.0));
            v.push(s(m, 22900.0, "PE", 300_000.0, 22970.0));
            v.push(s(m, 23000.0, "CE", 500_000.0, 22970.0));
        }
        let out = build_read(&ReadInputs {
            underlying: "NIFTY".into(),
            samples: v,
            prev_close: None,
        })
        .expect("read");
        assert_eq!(out["underlying"], "NIFTY");
        assert!(out["state"]["futures"].is_null());
        assert_eq!(out["state"]["flow"]["side"], serde_json::json!("put"));
    }
}
