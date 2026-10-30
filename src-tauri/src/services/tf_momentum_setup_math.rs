// services/tf_momentum_setup_math.rs
//! Pure-math port of the "Momentum Setup: Vijay Thakare Option Buying Scalping
//! Setup" chart indicator, so the same buy/sell signal that draws a triangle on
//! a chart a trader has open can be computed server-side for every symbol on the
//! TradeFinder Intraday Boost list, whether or not anyone has that symbol's
//! chart open.
//!
//! No I/O here. This module is one thing: given an ordered list of 5m bars,
//! decide whether the setup fires long or short on each bar, matching the
//! chart's own math bar-for-bar.
//!
//! Every primitive below (sma_seeded_ema, sma, stoch, atr, supertrend) is
//! transcribed from the openalgo-charts bundle, not reimplemented from a
//! textbook formula. Their NaN handling, seeding and warmup edge cases diverge
//! from the "usual" formula in ways that matter for matching what is drawn on
//! screen:
//!
//! - sma_seeded_ema's seed is the *unfiltered* mean of the first `period` raw
//!   values -- one NaN in the seed window poisons every value forever.
//! - sma is NaN-safe by counting non-finite entries in the rolling window, not
//!   by skipping them in the running sum.
//! - stoch returns NaN (not 0 or 50) on an exactly-zero range.
//! - atr is Wilder's RMA (seed = plain SMA of the first `period` true ranges,
//!   then a (prev*(n-1)+tr)/n recurrence), not a plain rolling SMA.
//! - supertrend's trend-flip test infers the previous trend from whether the
//!   previous bar's *chosen value* equalled the previous final-upper-band,
//!   rather than a stored boolean -- replicated exactly rather than simplified.
//!
//! ema_from_first_finite is not an openalgo-charts export -- it is defined
//! inline in the indicator file itself, only for the MACD signal line, which
//! needs its own seed to start at the first finite value of macdLine.

use std::collections::HashMap;

/// A single OHLCV bar. `time` is epoch seconds; `volume` may be missing.
#[derive(Clone, Debug)]
pub struct Bar {
    pub time: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: Option<f64>,
}

/// The indicator's own defaults -- the ones the user actually applied to the
/// chart. See `Settings`.
#[derive(Clone, Debug)]
pub struct Settings {
    pub st_atr_period: usize,
    pub st_factor: f64,
    pub macd_fast: usize,
    pub macd_slow: usize,
    pub macd_signal: usize,
    pub stoc_k_len: usize,
    pub stoc_k_smooth: usize,
    pub stoc_d_smooth: usize,
    pub stoc_overbought: f64,
    pub stoc_oversold: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            st_atr_period: 10,
            st_factor: 3.0,
            macd_fast: 3,
            macd_slow: 9,
            macd_signal: 15,
            stoc_k_len: 5,
            stoc_k_smooth: 3,
            stoc_d_smooth: 3,
            stoc_overbought: 60.0,
            stoc_oversold: 40.0,
        }
    }
}

/// One supertrend reading. `direction` is -1 (uptrend) or 1 (downtrend).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SuperTrend {
    pub value: f64,
    pub direction: i8,
}

/// Per-bar output arrays. `long_trigger[n-1]` / `short_trigger[n-1]` on the most
/// recently closed bar is what the caller treats as "signaling right now".
#[derive(Clone, Debug, Default)]
pub struct Signals {
    pub long_trigger: Vec<i8>,
    pub short_trigger: Vec<i8>,
    pub macd: Vec<f64>,
    pub signal: Vec<f64>,
    pub hist: Vec<f64>,
    pub stoc_k: Vec<f64>,
    pub stoc_d: Vec<f64>,
    pub supertrend_value: Vec<f64>,
    pub supertrend_direction: Vec<i8>,
    pub vwap: Vec<f64>,
}

const NAN: f64 = f64::NAN;

/// EMA seeded from the plain mean of the first `period` raw values (Pine's
/// ta.ema seeding). The seed is not NaN-filtered: one NaN in the seed window
/// poisons the seed, and the recurrence carries that poison forward forever.
pub fn sma_seeded_ema(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![NAN; n];
    if period == 0 || n < period {
        return out;
    }
    let seed: f64 = values[0..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = seed;
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut prev = seed;
    for i in period..n {
        prev = values[i] * alpha + prev * (1.0 - alpha);
        out[i] = prev;
    }
    out
}

/// sma_seeded_ema, but seeded from the first *finite* value of `values` rather
/// than index 0 -- used only for the MACD signal line, whose input (the MACD
/// line itself) starts with NaNs from its own EMA warmups.
pub fn ema_from_first_finite(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![NAN; n];
    let mut first = 0;
    while first < n && !values[first].is_finite() {
        first += 1;
    }
    if first >= n {
        return out;
    }
    let tail = sma_seeded_ema(&values[first..], period);
    for (i, v) in tail.into_iter().enumerate() {
        out[first + i] = v;
    }
    out
}

/// NaN-safe rolling mean: a window with any non-finite entry outputs NaN for
/// that bar (tracked by counting, not by summing NaN and poisoning forever the
/// way sma_seeded_ema deliberately does).
pub fn sma(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![NAN; n];
    if period == 0 || n < period {
        return out;
    }
    let mut total = 0.0;
    let mut nan_count: i64 = 0;
    for i in 0..n {
        let v = values[i];
        if v.is_finite() {
            total += v;
        } else {
            nan_count += 1;
        }
        if i >= period {
            let dropped = values[i - period];
            if dropped.is_finite() {
                total -= dropped;
            } else {
                nan_count -= 1;
            }
        }
        if i >= period - 1 {
            out[i] = if nan_count == 0 {
                total / period as f64
            } else {
                NAN
            };
        }
    }
    out
}

fn rolling_extreme(values: &[f64], length: usize, is_max: bool) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![NAN; n];
    if length == 0 {
        return out;
    }
    for i in (length - 1)..n {
        let window = &values[i + 1 - length..=i];
        if window.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let mut acc = window[0];
        for &v in &window[1..] {
            if (is_max && v > acc) || (!is_max && v < acc) {
                acc = v;
            }
        }
        out[i] = acc;
    }
    out
}

/// Raw %K = 100*(close-lowestLow)/(highestHigh-lowestLow) over `length` bars. An
/// exactly-zero range (a flat window) is NaN, not 0 or 50.
pub fn stoch(close: &[f64], high: &[f64], low: &[f64], length: usize) -> Vec<f64> {
    let n = close.len();
    let highest = rolling_extreme(high, length, true);
    let lowest = rolling_extreme(low, length, false);
    let mut out = vec![NAN; n];
    for i in 0..n {
        let (hh, ll) = (highest[i], lowest[i]);
        if !(hh.is_finite() && ll.is_finite()) {
            continue;
        }
        let rng = hh - ll;
        out[i] = if rng == 0.0 {
            NAN
        } else {
            100.0 * (close[i] - ll) / rng
        };
    }
    out
}

pub fn true_range(high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let n = high.len();
    let mut tr = vec![0.0; n];
    if n == 0 {
        return tr;
    }
    tr[0] = high[0] - low[0];
    for i in 1..n {
        tr[i] = (high[i] - low[i])
            .max((high[i] - close[i - 1]).abs())
            .max((low[i] - close[i - 1]).abs());
    }
    tr
}

/// Wilder's RMA: seed = plain SMA of the first `period` true ranges at index
/// period-1, then atr[i] = (atr[i-1]*(period-1) + tr[i]) / period.
pub fn atr(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let n = high.len();
    let mut out = vec![NAN; n];
    if period == 0 || n < period {
        return out;
    }
    let tr = true_range(high, low, close);
    let seed: f64 = tr[0..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = seed;
    let mut prev = seed;
    for i in period..n {
        prev = (prev * (period as f64 - 1.0) + tr[i]) / period as f64;
        out[i] = prev;
    }
    out
}

/// Returns one SuperTrend per bar. -1 means the bar is in an uptrend, tracked
/// off the ratcheting-up lower band; 1 means a downtrend tracked off the
/// ratcheting-down upper band. Bars before ATR warms up get {nan, 1} (the
/// array's own initial fill in the source, never overwritten for those bars).
pub fn supertrend(bars: &[Bar], atr_period: usize, factor: f64) -> Vec<SuperTrend> {
    let n = bars.len();
    let high: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let low: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let close: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let atr_vals = atr(&high, &low, &close, atr_period);

    let mut out = vec![
        SuperTrend {
            value: NAN,
            direction: 1
        };
        n
    ];
    let mut prev_upper = NAN;
    let mut prev_lower = NAN;
    let mut prev_value = NAN;
    let mut started = false;

    for i in 0..n {
        if !atr_vals[i].is_finite() {
            continue;
        }
        let hl2 = (high[i] + low[i]) / 2.0;
        let basic_upper = hl2 + factor * atr_vals[i];
        let basic_lower = hl2 - factor * atr_vals[i];

        let (final_upper, final_lower) = if started {
            let fu = if basic_upper < prev_upper || close[i - 1] > prev_upper {
                basic_upper
            } else {
                prev_upper
            };
            let fl = if basic_lower > prev_lower || close[i - 1] < prev_lower {
                basic_lower
            } else {
                prev_lower
            };
            (fu, fl)
        } else {
            (basic_upper, basic_lower)
        };

        let (value, direction) = if started && prev_value != prev_upper {
            // Previous bar tracked the lower band (uptrend).
            if close[i] >= final_lower {
                (final_lower, -1)
            } else {
                (final_upper, 1)
            }
        } else {
            // Previous bar tracked the upper band (downtrend), or this is the
            // first bar the state machine has ever seen.
            if close[i] <= final_upper {
                (final_upper, 1)
            } else {
                (final_lower, -1)
            }
        };

        out[i] = SuperTrend { value, direction };
        prev_upper = final_upper;
        prev_lower = final_lower;
        prev_value = value;
        started = true;
    }

    out
}

pub fn hlc3(bars: &[Bar]) -> Vec<f64> {
    bars.iter().map(|b| (b.high + b.low + b.close) / 3.0).collect()
}

pub fn ohlc4(bars: &[Bar]) -> Vec<f64> {
    bars.iter()
        .map(|b| (b.open + b.high + b.low + b.close) / 4.0)
        .collect()
}

/// The IST calendar day (as a day ordinal) for an epoch-seconds timestamp. IST
/// is a fixed +5:30 offset with no DST, so the day boundary is a pure integer
/// division once the offset is added -- no timezone database needed.
fn ist_day(epoch_seconds: f64) -> i64 {
    const IST_OFFSET: f64 = 5.5 * 3600.0;
    ((epoch_seconds + IST_OFFSET) / 86_400.0).floor() as i64
}

/// VWAP anchored to the IST calendar day (the indicator's default '1D' anchor).
/// A missing volume is NaN, which -- left unfiltered, exactly as the source
/// does -- poisons the running sums for the rest of that day.
pub fn daily_vwap(bars: &[Bar]) -> Vec<f64> {
    let n = bars.len();
    let mut out = vec![NAN; n];
    let h3 = hlc3(bars);
    let mut sum_pv = 0.0;
    let mut sum_v = 0.0;
    let mut prev_day: Option<i64> = None;
    for (i, b) in bars.iter().enumerate() {
        let day = ist_day(b.time);
        if prev_day != Some(day) {
            sum_pv = 0.0;
            sum_v = 0.0;
        }
        let vol = match b.volume {
            Some(v) if v.is_finite() => v,
            _ => NAN,
        };
        sum_pv += h3[i] * vol;
        sum_v += vol;
        out[i] = if sum_v > 0.0 { sum_pv / sum_v } else { NAN };
        prev_day = Some(day);
    }
    out
}

/// The Continuous-mode two-step arm/fire state machine shared by both the long
/// and short sides: cond1 arms, cond2 (while armed) fires, firing re-arms on the
/// next bar. Run forward once over the whole array -- the state has no notion of
/// a day boundary, only of array position, so feeding it one continuous
/// multi-day bar list is the whole answer.
fn continuous_trigger(cond1: &[bool], cond2: &[bool]) -> Vec<i8> {
    let mut state = 0u8;
    let n = cond1.len();
    let mut out = vec![0i8; n];
    for i in 0..n {
        if state == 2 {
            state = 0;
        }
        if cond1[i] && state == 0 {
            state = 1;
        }
        if cond2[i] && state == 1 {
            state = 2;
        }
        out[i] = if state == 2 { 1 } else { 0 };
    }
    out
}

/// Orchestrates the indicator's calc(): Supertrend, daily VWAP, MACD 3/9/15 and
/// Stochastic 5/3/3 (the indicator's own defaults) feed the entry conditions,
/// and the Continuous-mode state machine decides whether long/short fires on
/// each bar. EMAs 20/50/100/200 are display-only in the source and are not
/// computed here. Every "Additional Filter" is off by default and stays off --
/// only Continuous mode is ported, not Flip.
///
/// Feed a multi-day continuous series (not "just today") so the arm/fire state
/// has already settled past warmup noise and can carry an in-progress setup
/// across the day boundary.
pub fn momentum_setup_signals(bars: &[Bar], settings: &Settings) -> Signals {
    let s = settings;
    let n = bars.len();
    let mut out = Signals {
        long_trigger: vec![0; n],
        short_trigger: vec![0; n],
        macd: vec![NAN; n],
        signal: vec![NAN; n],
        hist: vec![NAN; n],
        stoc_k: vec![NAN; n],
        stoc_d: vec![NAN; n],
        supertrend_value: vec![NAN; n],
        supertrend_direction: vec![1; n],
        vwap: vec![NAN; n],
    };
    if n == 0 {
        return out;
    }

    let close: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let high: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let low: Vec<f64> = bars.iter().map(|b| b.low).collect();

    let macd_fast = sma_seeded_ema(&close, s.macd_fast);
    let macd_slow = sma_seeded_ema(&close, s.macd_slow);
    let macd_line: Vec<f64> = (0..n)
        .map(|i| {
            if macd_fast[i].is_finite() && macd_slow[i].is_finite() {
                macd_fast[i] - macd_slow[i]
            } else {
                NAN
            }
        })
        .collect();
    let signal_line = ema_from_first_finite(&macd_line, s.macd_signal);
    let hist_line: Vec<f64> = (0..n)
        .map(|i| {
            if macd_line[i].is_finite() && signal_line[i].is_finite() {
                macd_line[i] - signal_line[i]
            } else {
                NAN
            }
        })
        .collect();

    let stoc_k_raw = stoch(&close, &high, &low, s.stoc_k_len);
    let stoc_k = sma(&stoc_k_raw, s.stoc_k_smooth);
    let stoc_d = sma(&stoc_k, s.stoc_d_smooth);

    let st = supertrend(bars, s.st_atr_period, s.st_factor);
    let vwap = daily_vwap(bars);

    let mut short_cond1 = vec![false; n];
    let mut short_cond2 = vec![false; n];
    let mut long_cond1 = vec![false; n];
    let mut long_cond2 = vec![false; n];

    for i in 0..n {
        let (macd, sig, hist) = (macd_line[i], signal_line[i], hist_line[i]);
        let (k, d) = (stoc_k[i], stoc_d[i]);
        let (stv, anchor, c) = (st[i].value, vwap[i], close[i]);

        let core_finite = [macd, sig, hist, k, d, stv, anchor, c]
            .iter()
            .all(|x| x.is_finite());

        short_cond1[i] = core_finite && macd > 0.0;
        long_cond1[i] = core_finite && macd < 0.0;

        short_cond2[i] = core_finite
            && c < stv
            && c < anchor
            && k < s.stoc_overbought
            && k < d
            && macd < 0.0
            && macd < sig
            && macd < hist;
        long_cond2[i] = core_finite
            && c > stv
            && c > anchor
            && k > s.stoc_oversold
            && k > d
            && macd > 0.0
            && macd > sig
            && macd > hist;

        out.macd[i] = macd;
        out.signal[i] = sig;
        out.hist[i] = hist;
        out.stoc_k[i] = k;
        out.stoc_d[i] = d;
        out.supertrend_value[i] = stv;
        out.supertrend_direction[i] = st[i].direction;
        out.vwap[i] = anchor;
    }

    out.short_trigger = continuous_trigger(&short_cond1, &short_cond2);
    out.long_trigger = continuous_trigger(&long_cond1, &long_cond2);
    out
}

/// Settings overlay from a loose string->f64 map (the shape a stored indicator
/// config arrives in), falling back to the indicator defaults for any key the
/// map omits. Non-integer values for the integer fields are truncated, matching
/// the chart's own coercion of its numeric inputs.
pub fn settings_from_map(overrides: &HashMap<String, f64>) -> Settings {
    let d = Settings::default();
    let u = |k: &str, fallback: usize| -> usize {
        overrides.get(k).map(|v| *v as usize).unwrap_or(fallback)
    };
    let f = |k: &str, fallback: f64| -> f64 { overrides.get(k).copied().unwrap_or(fallback) };
    Settings {
        st_atr_period: u("st_atr_period", d.st_atr_period),
        st_factor: f("st_factor", d.st_factor),
        macd_fast: u("macd_fast", d.macd_fast),
        macd_slow: u("macd_slow", d.macd_slow),
        macd_signal: u("macd_signal", d.macd_signal),
        stoc_k_len: u("stoc_k_len", d.stoc_k_len),
        stoc_k_smooth: u("stoc_k_smooth", d.stoc_k_smooth),
        stoc_d_smooth: u("stoc_d_smooth", d.stoc_d_smooth),
        stoc_overbought: f("stoc_overbought", d.stoc_overbought),
        stoc_oversold: f("stoc_oversold", d.stoc_oversold),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn sma_is_nan_until_the_window_fills_then_the_plain_mean() {
        let v = [1.0, 2.0, 3.0, 4.0];
        let out = sma(&v, 2);
        assert!(out[0].is_nan());
        approx(out[1], 1.5);
        approx(out[2], 2.5);
        approx(out[3], 3.5);
    }

    #[test]
    fn sma_window_with_a_nan_is_nan_but_recovers_once_it_rolls_off() {
        // A non-finite entry blanks only the windows that contain it; counting
        // (not summing) is what lets the mean recover afterwards.
        let v = [1.0, f64::NAN, 3.0, 4.0];
        let out = sma(&v, 2);
        assert!(out[1].is_nan());
        assert!(out[2].is_nan());
        approx(out[3], 3.5);
    }

    #[test]
    fn sma_seeded_ema_seeds_on_the_plain_mean_then_recurs() {
        // Seed at index period-1 is the mean of the first `period` raw values;
        // thereafter alpha = 2/(period+1).
        let v = [2.0, 4.0, 6.0, 8.0];
        let out = sma_seeded_ema(&v, 2);
        assert!(out[0].is_nan());
        approx(out[1], 3.0); // (2+4)/2
        let alpha = 2.0 / 3.0;
        approx(out[2], 6.0 * alpha + 3.0 * (1.0 - alpha));
        approx(out[3], 8.0 * alpha + out[2] * (1.0 - alpha));
    }

    #[test]
    fn sma_seeded_ema_seed_nan_poisons_every_later_value() {
        let v = [1.0, f64::NAN, 3.0, 4.0, 5.0];
        let out = sma_seeded_ema(&v, 2);
        for x in &out[1..] {
            assert!(x.is_nan(), "a NaN in the seed window must poison forever");
        }
    }

    #[test]
    fn ema_from_first_finite_skips_the_leading_nans() {
        let v = [f64::NAN, f64::NAN, 2.0, 4.0, 6.0];
        let out = ema_from_first_finite(&v, 2);
        assert!(out[0].is_nan());
        assert!(out[1].is_nan());
        assert!(out[2].is_nan()); // still inside the seed window of the tail
        approx(out[3], 3.0); // seed = (2+4)/2 at the tail's period-1
    }

    #[test]
    fn stoch_is_zero_at_the_low_and_a_hundred_at_the_high() {
        let close = [10.0, 20.0, 15.0];
        let high = [10.0, 20.0, 20.0];
        let low = [10.0, 10.0, 10.0];
        let out = stoch(&close, &high, &low, 2);
        assert!(out[0].is_nan());
        approx(out[1], 100.0); // close 20 at top of 10..20
        approx(out[2], 50.0); // close 15 at midpoint of 10..20
    }

    #[test]
    fn stoch_flat_window_is_nan_not_fifty() {
        let close = [5.0, 5.0, 5.0];
        let high = [5.0, 5.0, 5.0];
        let low = [5.0, 5.0, 5.0];
        let out = stoch(&close, &high, &low, 2);
        assert!(out[1].is_nan());
        assert!(out[2].is_nan());
    }

    #[test]
    fn true_range_first_bar_is_the_bar_range_then_includes_the_gap() {
        let high = [11.0, 13.0];
        let low = [9.0, 12.0];
        let close = [10.0, 12.5];
        let tr = true_range(&high, &low, &close);
        approx(tr[0], 2.0); // 11-9
        // max(13-12, |13-10|, |12-10|) = 3 (the gap up from prev close 10)
        approx(tr[1], 3.0);
    }

    #[test]
    fn atr_seeds_on_sma_then_follows_wilders_recurrence() {
        let high = [11.0, 12.0, 13.0, 14.0];
        let low = [9.0, 10.0, 11.0, 12.0];
        let close = [10.0, 11.0, 12.0, 13.0];
        let tr = true_range(&high, &low, &close);
        let out = atr(&high, &low, &close, 2);
        assert!(out[0].is_nan());
        let seed = (tr[0] + tr[1]) / 2.0;
        approx(out[1], seed);
        approx(out[2], (seed * 1.0 + tr[2]) / 2.0);
        let a2 = (seed * 1.0 + tr[2]) / 2.0;
        approx(out[3], (a2 * 1.0 + tr[3]) / 2.0);
    }

    #[test]
    fn daily_vwap_resets_on_the_ist_day_boundary() {
        // Two bars one IST day apart: the second day's VWAP must ignore the
        // first day's volume entirely.
        let day = 86_400.0;
        let bars = vec![
            Bar { time: 0.0, open: 10.0, high: 10.0, low: 10.0, close: 10.0, volume: Some(100.0) },
            Bar { time: day, open: 20.0, high: 20.0, low: 20.0, close: 20.0, volume: Some(50.0) },
        ];
        let out = daily_vwap(&bars);
        approx(out[0], 10.0);
        approx(out[1], 20.0); // fresh day: only bar 2's own hlc3
    }

    #[test]
    fn continuous_trigger_arms_then_fires_then_rearms() {
        // cond1 arms, cond2 fires while armed, and the bar after a fire re-arms.
        let cond1 = [true, false, false, true, false];
        let cond2 = [false, true, true, false, true];
        let out = continuous_trigger(&cond1, &cond2);
        // i0 arm; i1 fire(1); i2 state cleared, no cond1 so no fire(0);
        // i3 arm; i4 fire(1).
        assert_eq!(out, vec![0, 1, 0, 0, 1]);
    }

    #[test]
    fn settings_from_map_overlays_only_the_given_keys() {
        let mut m = HashMap::new();
        m.insert("st_factor".to_string(), 2.0);
        m.insert("macd_fast".to_string(), 5.0);
        let s = settings_from_map(&m);
        approx(s.st_factor, 2.0);
        assert_eq!(s.macd_fast, 5);
        assert_eq!(s.macd_slow, 9); // untouched default
        approx(s.stoc_overbought, 60.0); // untouched default
    }

    #[test]
    fn momentum_setup_signals_is_all_zero_triggers_through_warmup() {
        // With fewer bars than the slowest warmup, nothing can fire and every
        // core series stays NaN -- the triggers must all be 0, never spurious.
        let bars: Vec<Bar> = (0..5)
            .map(|i| Bar {
                time: i as f64 * 300.0,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: Some(1000.0),
            })
            .collect();
        let out = momentum_setup_signals(&bars, &Settings::default());
        assert!(out.long_trigger.iter().all(|&x| x == 0));
        assert!(out.short_trigger.iter().all(|&x| x == 0));
        assert_eq!(out.long_trigger.len(), 5);
    }

    #[test]
    fn empty_input_returns_empty_arrays() {
        let out = momentum_setup_signals(&[], &Settings::default());
        assert!(out.long_trigger.is_empty());
        assert!(out.macd.is_empty());
    }
}
