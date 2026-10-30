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
}
