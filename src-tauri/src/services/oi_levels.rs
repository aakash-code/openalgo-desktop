//! Option Levels: support/resistance zones, a strength heatmap (put/call
//! walls), touches and measured hold rates, derived minute-by-minute from the
//! recorded option OI. Port of the fork's `services/oi_levels/core.py`.
//!
//! Gamma is deferred: the fork injects an option-greeks callable and a
//! gamma-flip search; here `build_levels` runs with neither, so the gamma block
//! is null and everything else (zones, walls, touches, hold rates) stands. The
//! greeks wiring is a follow-up.
//!
//! Pure (no I/O). NOT yet verified against a running feed -- compiles and the
//! pure helpers are unit-tested, but the stateful zone/touch logic needs a live
//! Desktop to confirm.

use crate::services::market_read_core::{
    self as mr, level_events, near, Book, HOLD_MINUTES, LEVEL_STEPS, SIGNIFICANT_SHARE, WEAKEN_SHARE,
};
use crate::services::oi_flow_analysis::Sample;
use chrono::NaiveDateTime;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

const LEAD_SHARE: f64 = 0.10;
const ZONE_STRIKES: usize = 3;
const CHANGE_WINDOW: i64 = 10;
const TOUCH_SHARE: f64 = 0.0004;
const BOUNCE_SHARE: f64 = 0.0012;
const BREAK_SHARE: f64 = 0.0008;
const AFTER_MINUTES: i64 = 20;
const TOUCH_COOLDOWN: i64 = 15;
const MIN_JUDGED: i64 = 20;

const SUPPORT: &str = "support";
const RESISTANCE: &str = "resistance";

fn chance() -> f64 {
    BREAK_SHARE / (BOUNCE_SHARE + BREAK_SHARE)
}

fn side_of(role: &str) -> &'static str {
    if role == SUPPORT {
        "PE"
    } else {
        "CE"
    }
}
fn opposite(side: &str) -> &'static str {
    if side == "PE" {
        "CE"
    } else {
        "PE"
    }
}
fn ss(side: &str) -> &'static str {
    if side == "CE" {
        "CE"
    } else {
        "PE"
    }
}
fn turn_kind(role: &str) -> &'static str {
    if role == SUPPORT {
        "support_to_resistance"
    } else {
        "resistance_to_support"
    }
}

fn oi_at(book: &Book, t: &NaiveDateTime, side: &str, k: f64) -> Option<f64> {
    book.legs[t].get(&(ss(side), k.to_bits())).map(|l| l.0)
}
/// Inputs to [`build_levels`]. Gamma inputs (greeks/flip) are omitted here.
pub struct LevelInputs {
    pub underlying: String,
    pub samples: Vec<Sample>,
    pub expiry_day: bool,
    pub past_touches: Vec<Touch>,
}

/// Open interest on one side, on the side of the price that side defends.
/// Keyed by strike bits so it stays ordered.
fn own(book: &Book, t: &NaiveDateTime, side: &str) -> BTreeMap<u64, f64> {
    let spot = book.spot[t];
    let half = book.step / 2.0;
    let mut out = BTreeMap::new();
    for k in near(book, t, LEVEL_STEPS) {
        let Some(oi) = oi_at(book, t, side, k) else { continue };
        let keep = if side == "PE" { k <= spot + half } else { k >= spot - half };
        if keep {
            out.insert(k.to_bits(), oi);
        }
    }
    out
}

/// Runs of neighbouring strong strikes, biggest total first.
fn clusters(ois: &BTreeMap<u64, f64>, step: f64) -> Vec<(Vec<f64>, f64)> {
    if ois.is_empty() {
        return Vec::new();
    }
    let biggest = ois.values().cloned().fold(0.0_f64, f64::max);
    let mut strong: Vec<f64> = ois
        .iter()
        .filter(|(_, &oi)| oi >= SIGNIFICANT_SHARE * biggest)
        .map(|(b, _)| f64::from_bits(*b))
        .collect();
    strong.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut groups: Vec<Vec<f64>> = Vec::new();
    for k in strong {
        if let Some(last) = groups.last_mut() {
            if k - *last.last().unwrap() <= step + 1e-9 && last.len() < ZONE_STRIKES {
                last.push(k);
                continue;
            }
        }
        groups.push(vec![k]);
    }
    let mut out: Vec<(Vec<f64>, f64)> = groups
        .into_iter()
        .map(|g| {
            let total: f64 = g.iter().map(|k| ois[&k.to_bits()]).sum();
            (g, total)
        })
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    out
}

#[derive(Default)]
struct Track {
    anchor: Option<f64>,
    since: Option<NaiveDateTime>,
    challenger: Option<f64>,
    challenger_minutes: i64,
    beyond_minutes: i64,
}

/// When the market read saw each strike turn, by (role it turned from, strike bits).
fn turns(book: &Book, expiry_day: bool) -> HashMap<(&'static str, u64), Vec<NaiveDateTime>> {
    let (events, _) = level_events(book, expiry_day);
    let mut out: HashMap<(&'static str, u64), Vec<NaiveDateTime>> = HashMap::new();
    for e in events {
        for role in [SUPPORT, RESISTANCE] {
            if e.kind == turn_kind(role) {
                // id = "{kind}:{strike_str}:{HHMM}"
                if let Some(part) = e.id.split(':').nth(1) {
                    if let Ok(strike) = part.replace(',', "").parse::<f64>() {
                        out.entry((role_static(role), strike.to_bits())).or_default().push(e.time);
                    }
                }
            }
        }
    }
    out
}

fn role_static(role: &str) -> &'static str {
    if role == SUPPORT {
        SUPPORT
    } else {
        RESISTANCE
    }
}

#[allow(clippy::too_many_arguments)]
fn zone(
    book: &Book,
    t: &NaiveDateTime,
    role: &str,
    anchor: f64,
    own_map: &BTreeMap<u64, f64>,
    peak: &HashMap<(&'static str, u64), f64>,
    expiry_day: bool,
    index: usize,
) -> Value {
    let side = side_of(role);
    let mut pool = own_map.clone();
    pool.entry(anchor.to_bits()).or_insert_with(|| oi_at(book, t, side, anchor).unwrap_or(0.0));
    let group = clusters(&pool, book.step)
        .into_iter()
        .map(|(g, _)| g)
        .find(|g| g.iter().any(|k| (*k - anchor).abs() < 1e-9))
        .unwrap_or_else(|| vec![anchor]);
    let total: f64 = own_map.values().sum::<f64>().max(1.0);
    let anchor_oi = oi_at(book, t, side, anchor).unwrap_or(0.0);
    let top = peak.get(&(side, anchor.to_bits())).copied().unwrap_or(anchor_oi).max(1.0);
    let earlier_t = book.times[index.saturating_sub(CHANGE_WINDOW as usize)];
    let own_change = anchor_oi - earlier_oi(book, &earlier_t, side, anchor, anchor_oi);
    let opp_now = oi_at(book, t, opposite(side), anchor).unwrap_or(0.0);
    let opp_change = opp_now - earlier_oi(book, &earlier_t, opposite(side), anchor, opp_now);
    let weakening = !expiry_day && anchor_oi <= (1.0 - WEAKEN_SHARE) * top;
    let group_oi: f64 = group.iter().map(|k| pool.get(&k.to_bits()).copied().unwrap_or(0.0)).sum();
    json!({
        "role": role,
        "anchor": anchor,
        "low": group.iter().cloned().fold(f64::INFINITY, f64::min),
        "high": group.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        "strikes": group,
        "oi": group_oi,
        "size": round3(group_oi / total),
        "from_peak_pct": round1((anchor_oi / top - 1.0) * 100.0),
        "change_own": own_change,
        "change_opposite": opp_change,
        "status": if weakening { "weakening" } else { "holding" },
    })
}

fn earlier_oi(book: &Book, t: &NaiveDateTime, side: &str, k: f64, fallback: f64) -> f64 {
    book.legs.get(t).and_then(|l| l.get(&(ss(side), k.to_bits()))).map(|l| l.0).unwrap_or(fallback)
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Per minute, the primary zone on each side, plus every change of anchor.
#[allow(clippy::type_complexity)]
fn levels(
    book: &Book,
    expiry_day: bool,
) -> (BTreeMap<NaiveDateTime, HashMap<&'static str, Option<Value>>>, Vec<Value>) {
    let turns = turns(book, expiry_day);
    let mut peak: HashMap<(&'static str, u64), f64> = HashMap::new();
    let mut tracks: HashMap<&'static str, Track> =
        HashMap::from([(SUPPORT, Track::default()), (RESISTANCE, Track::default())]);
    let mut history: BTreeMap<NaiveDateTime, HashMap<&'static str, Option<Value>>> = BTreeMap::new();
    let mut changes: Vec<Value> = Vec::new();
    let mut last_zone: HashMap<&'static str, Value> = HashMap::new();

    for (i, t) in book.times.iter().enumerate() {
        let spot = book.spot[t];
        for (key, leg) in book.legs[t].iter() {
            let e = peak.entry(*key).or_insert(0.0);
            *e = e.max(leg.0);
        }
        let through = BREAK_SHARE * spot;
        let mut minute: HashMap<&'static str, Option<Value>> = HashMap::new();
        for role in [SUPPORT, RESISTANCE] {
            let side = side_of(role);
            let own_map = own(book, t, side);
            if own_map.is_empty() {
                minute.insert(role_static(role), last_zone.get(role_static(role)).cloned());
                continue;
            }
            let top = f64::from_bits(
                *own_map.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0,
            );
            let tr = tracks.get_mut(role_static(role)).unwrap();
            if tr.anchor.is_none() {
                tr.anchor = Some(top);
                tr.since = Some(*t);
            }
            let anchor = tr.anchor.unwrap();
            let beyond = if side == "PE" { spot < anchor - through } else { spot > anchor + through };
            tr.beyond_minutes = if beyond { tr.beyond_minutes + 1 } else { 0 };
            let anchor_oi = oi_at(book, t, side, anchor).unwrap_or(0.0);

            if (anchor - spot).abs() > LEVEL_STEPS as f64 * book.step + 1e-9 && top != anchor {
                changes.push(json!({"time": iso(*t), "role": role, "from": anchor, "to": top, "kind": "moved"}));
                tr.anchor = Some(top);
                tr.since = Some(*t);
                tr.challenger = None;
                tr.challenger_minutes = 0;
                tr.beyond_minutes = 0;
            } else if tr.beyond_minutes >= HOLD_MINUTES {
                let since = tr.since.unwrap();
                let turned = turns
                    .get(&(role_static(role), anchor.to_bits()))
                    .map(|xs| xs.iter().any(|x| since <= *x && *x <= *t))
                    .unwrap_or(false);
                changes.push(json!({"time": iso(*t), "role": role, "from": anchor, "to": top,
                    "kind": if turned { "turned" } else { "broken" }}));
                tr.anchor = Some(top);
                tr.since = Some(*t);
                tr.challenger = None;
                tr.challenger_minutes = 0;
                tr.beyond_minutes = 0;
            } else if top != anchor && own_map[&top.to_bits()] >= (1.0 + LEAD_SHARE) * anchor_oi {
                if tr.challenger == Some(top) {
                    tr.challenger_minutes += 1;
                } else {
                    tr.challenger = Some(top);
                    tr.challenger_minutes = 1;
                }
                if tr.challenger_minutes >= HOLD_MINUTES {
                    changes.push(json!({"time": iso(*t), "role": role, "from": anchor, "to": top, "kind": "moved"}));
                    tr.anchor = Some(top);
                    tr.since = Some(*t);
                    tr.challenger = None;
                    tr.challenger_minutes = 0;
                }
            } else {
                tr.challenger = None;
                tr.challenger_minutes = 0;
            }

            let anchor = tracks[role_static(role)].anchor.unwrap();
            let since = tracks[role_static(role)].since.unwrap();
            let mut z = zone(book, t, role, anchor, &own_map, &peak, expiry_day, i);
            z["since"] = json!(iso(since));
            // The second cluster not containing the anchor.
            let second = clusters(&own_map, book.step).into_iter().find(|(g, _)| {
                !g.iter().any(|k| (*k - anchor).abs() < 1e-9)
                    && !(g.iter().cloned().fold(f64::INFINITY, f64::min) <= anchor
                        && anchor <= g.iter().cloned().fold(f64::NEG_INFINITY, f64::max))
            });
            z["next"] = match second {
                Some((g, oi)) => json!({
                    "low": g.iter().cloned().fold(f64::INFINITY, f64::min),
                    "high": g.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
                    "oi": oi
                }),
                None => Value::Null,
            };
            last_zone.insert(role_static(role), z.clone());
            minute.insert(role_static(role), Some(z));
        }
        history.insert(*t, minute);
    }
    (history, changes)
}

fn iso(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// One recorded touch of a zone or round strike, with what followed.
#[derive(Clone)]
pub struct Touch {
    pub kind: String,   // "zone" | "round"
    pub status: String, // "holding" | "weakening" | "round"
    pub outcome: String,
}

fn outcome(book: &Book, i: usize, role: &str, low: f64, high: f64) -> &'static str {
    let t = book.times[i];
    let spot = book.spot[&t];
    let bounce = BOUNCE_SHARE * spot;
    let through = BREAK_SHARE * spot;
    let end = t + chrono::Duration::minutes(AFTER_MINUTES);
    for later in &book.times[i + 1..] {
        if *later > end {
            return "neither";
        }
        let p = book.spot[later];
        if role == SUPPORT {
            if p >= high + bounce {
                return "held";
            }
            if p <= low - through {
                return "broke";
            }
        } else {
            if p <= low - bounce {
                return "held";
            }
            if p >= high + through {
                return "broke";
            }
        }
    }
    "pending"
}

/// Every touch of a zone and of every round strike today, with what followed.
fn touches(
    book: &Book,
    history: &BTreeMap<NaiveDateTime, HashMap<&'static str, Option<Value>>>,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut last: HashMap<(String, String, u64), NaiveDateTime> = HashMap::new();
    let round_step = 2.0 * book.step;
    for i in 1..book.times.len() {
        let t = book.times[i];
        let prev_t = book.times[i - 1];
        let spot = book.spot[&t];
        let prev = book.spot[&prev_t];
        let tol = TOUCH_SHARE * spot;
        // tests: (kind, role, anchor, low, high, status)
        let mut tests: Vec<(&str, &str, f64, f64, f64, String)> = Vec::new();
        if let Some(minute) = history.get(&prev_t) {
            for role in [SUPPORT, RESISTANCE] {
                if let Some(Some(z)) = minute.get(role_static(role)) {
                    tests.push((
                        "zone",
                        role,
                        z["anchor"].as_f64().unwrap_or(0.0),
                        z["low"].as_f64().unwrap_or(0.0),
                        z["high"].as_f64().unwrap_or(0.0),
                        z["status"].as_str().unwrap_or("holding").to_string(),
                    ));
                }
            }
        }
        for k in near(book, &t, LEVEL_STEPS) {
            if (k / round_step - (k / round_step).round()).abs() < 1e-9 {
                let role = if k <= spot { SUPPORT } else { RESISTANCE };
                tests.push(("round", role, k, k, k, "round".to_string()));
            }
        }
        for (kind, role, anchor, low, high, status) in tests {
            let touched = if role == SUPPORT {
                low - tol <= spot && spot <= high + tol && prev > high + tol
            } else {
                low - tol <= spot && spot <= high + tol && prev < low - tol
            };
            let key = (kind.to_string(), role.to_string(), anchor.to_bits());
            if !touched {
                continue;
            }
            if let Some(prev_touch) = last.get(&key) {
                if (t - *prev_touch).num_minutes() < TOUCH_COOLDOWN {
                    continue;
                }
            }
            last.insert(key, t);
            out.push(json!({
                "time": iso(t),
                "kind": kind,
                "role": role,
                "strike": anchor,
                "status": status,
                "outcome": outcome(book, i, role, low, high),
            }));
        }
    }
    out
}

/// Held against broke, by kind and status. Touches that did neither are left out.
fn hold_rates(touches: &[Value], past: &[Touch]) -> Value {
    let mut rows: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut add = |kind: &str, status: &str, outcome: &str| {
        if outcome != "held" && outcome != "broke" {
            return;
        }
        let name = if kind == "round" { "round".to_string() } else { format!("zone_{status}") };
        let e = rows.entry(name).or_insert((0, 0));
        e.0 += 1;
        if outcome == "held" {
            e.1 += 1;
        }
    };
    for p in past {
        add(&p.kind, &p.status, &p.outcome);
    }
    for x in touches {
        add(
            x["kind"].as_str().unwrap_or(""),
            x["status"].as_str().unwrap_or(""),
            x["outcome"].as_str().unwrap_or(""),
        );
    }
    let mut out = Map::new();
    for (name, (n, held)) in rows {
        out.insert(
            name,
            json!({
                "n": n,
                "held": held,
                "rate": if n > 0 { Some(round3(held as f64 / n as f64)) } else { None },
                "enough": n >= MIN_JUDGED,
            }),
        );
    }
    Value::Object(out)
}

/// Per near-the-money strike at one minute: put/call OI and their change.
fn walls(book: &Book, index: usize) -> Vec<Value> {
    let t = book.times[index];
    let earlier_t = book.times[index.saturating_sub(CHANGE_WINDOW as usize)];
    let mut out = Vec::new();
    for k in near(book, &t, LEVEL_STEPS) {
        let put = oi_at(book, &t, "PE", k).unwrap_or(0.0);
        let call = oi_at(book, &t, "CE", k).unwrap_or(0.0);
        out.push(json!({
            "strike": k,
            "put": put,
            "call": call,
            "put_change": put - earlier_oi(book, &earlier_t, "PE", k, put),
            "call_change": call - earlier_oi(book, &earlier_t, "CE", k, call),
        }));
    }
    out
}

fn brief(zone: Option<&Value>) -> Value {
    match zone {
        Some(z) => json!([z["low"], z["high"], z["status"], z["size"]]),
        None => Value::Null,
    }
}

/// The zones for the day so far, or None when fewer than two minutes are held.
pub fn build_levels(inputs: &LevelInputs) -> Option<Value> {
    let book = mr::book(&inputs.samples)?;
    let (history, changes) = levels(&book, inputs.expiry_day);
    let touches_today = touches(&book, &history);
    let stats = hold_rates(&touches_today, &inputs.past_touches);

    let t = *book.times.last().unwrap();
    let mut zones = Vec::new();
    for role in [SUPPORT, RESISTANCE] {
        let Some(Some(z)) = history.get(&t).and_then(|m| m.get(role_static(role))).map(|o| o.as_ref())
        else {
            continue;
        };
        let mut z = z.clone();
        let status = z["status"].as_str().unwrap_or("holding");
        let key = format!("zone_{status}");
        z["measured"] = stats.get(&key).cloned().unwrap_or(json!({"n":0,"held":0,"rate":null,"enough":false}));
        z["high_gamma"] = json!(false); // gamma deferred
        let anchor = z["anchor"].as_f64().unwrap_or(0.0);
        z["touches_today"] = json!(touches_today
            .iter()
            .filter(|x| x["kind"] == "zone" && x["role"] == role && x["strike"].as_f64() == Some(anchor))
            .cloned()
            .collect::<Vec<_>>());
        zones.push(z);
    }

    let history_brief: Vec<Value> = book
        .times
        .iter()
        .map(|m| {
            let minute = history.get(m);
            json!({
                "time": iso(*m),
                SUPPORT: brief(minute.and_then(|x| x.get(SUPPORT)).and_then(|o| o.as_ref())),
                RESISTANCE: brief(minute.and_then(|x| x.get(RESISTANCE)).and_then(|o| o.as_ref())),
            })
        })
        .collect();

    Some(json!({
        "underlying": inputs.underlying,
        "minute": iso(t),
        "spot": book.spot[&t],
        "step": book.step,
        "thresholds": {
            "touch_pct": TOUCH_SHARE * 100.0,
            "bounce_pct": BOUNCE_SHARE * 100.0,
            "break_pct": BREAK_SHARE * 100.0,
            "after_min": AFTER_MINUTES,
            "chance": round3(chance()),
            "min_judged": MIN_JUDGED,
        },
        "zones": zones,
        "walls": walls(&book, book.times.len() - 1),
        "history": history_brief,
        "changes": changes,
        "gamma": { "forward": null_v(), "top_strike": null_v(), "flip": null_v(), "series": [] },
        "touches": touches_today,
        "stats": stats,
    }))
}

fn null_v() -> Value {
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(min: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 21).unwrap().and_hms_opt(9, 15, 0).unwrap()
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
    fn clusters_groups_adjacent_strong_strikes() {
        let mut ois = BTreeMap::new();
        for (k, oi) in [(22900.0, 1_000_000.0), (22950.0, 900_000.0), (23200.0, 300_000.0)] {
            ois.insert((k as f64).to_bits(), oi);
        }
        let c = clusters(&ois, 50.0);
        // 22900+22950 cluster is strong and adjacent; 23200 is below the share.
        assert_eq!(c[0].0, vec![22900.0, 22950.0]);
    }

    #[test]
    fn build_levels_reports_a_support_zone() {
        let mut v = Vec::new();
        for m in 0..6 {
            v.push(s(m, 22900.0, "PE", 1_000_000.0, 22970.0));
            v.push(s(m, 22950.0, "PE", 400_000.0, 22970.0));
            v.push(s(m, 23000.0, "CE", 800_000.0, 22970.0));
            v.push(s(m, 23050.0, "CE", 300_000.0, 22970.0));
        }
        let out = build_levels(&LevelInputs {
            underlying: "NIFTY".into(),
            samples: v,
            expiry_day: false,
            past_touches: Vec::new(),
        })
        .expect("levels");
        assert_eq!(out["underlying"], "NIFTY");
        assert!(out["zones"].as_array().unwrap().iter().any(|z| z["role"] == "support"));
    }
}
