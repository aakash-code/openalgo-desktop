//! TradeFinder Intraday Boost recorder: once a minute during market hours it
//! fetches the ranked lists and stores one snapshot per list, so the movement
//! engine can later reconstruct each stock's path to its rank. The store and the
//! read routes (`services/tf_boost_db`, `services/tf_boost_movement_service`)
//! are what consume this; a `record_now` with no live TradeFinder token stores
//! nothing and is harmless, so the tick is safe to leave running.
//!
//! Opt-in via `TF_BOOST_RECORDER_ENABLED=true`. Mirrors the Writer Flow
//! recorder's shape (owned task, five-seconds-past-the-minute cadence, weekday
//! market-hours gate). Port of the fork's `services/tf_boost_snapshot_service`
//! recorder loop, write path only.

use crate::services::{tf_boost_db, tf_boost_movement_service};
use crate::state::AppState;
use chrono::{Datelike, NaiveTime, Timelike};
use chrono_tz::Asia::Kolkata;
use std::sync::Arc;
use std::time::Duration;

fn recorder_enabled() -> bool {
    matches!(
        std::env::var("TF_BOOST_RECORDER_ENABLED").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

/// Weekday, 09:15-15:40 IST. The Intraday Boost lists are an equity-market
/// intraday feed; outside those hours the ranked list is stale, so a tick is
/// skipped rather than storing a repeat of the close.
fn in_market_hours(now_ist: chrono::NaiveDateTime) -> bool {
    let wd = now_ist.weekday().number_from_monday();
    if wd > 5 {
        return false;
    }
    let t = now_ist.time();
    t >= NaiveTime::from_hms_opt(9, 15, 0).unwrap() && t <= NaiveTime::from_hms_opt(15, 40, 0).unwrap()
}

/// Start the recorder (an owned task; the process exit stops it). A no-op unless
/// `TF_BOOST_RECORDER_ENABLED=true`.
pub fn start(ctx: &Arc<AppState>) {
    if !recorder_enabled() {
        return;
    }
    if let Err(e) = tf_boost_db::init(&ctx.data_dir) {
        tracing::warn!("TF Boost recorder: could not open its store: {e}");
        return;
    }
    let ctx = ctx.clone();
    tokio::spawn(async move {
        tracing::info!("TF Boost recorder started");
        loop {
            sleep_to_next_minute().await;
            let now_ist = ctx.now().with_timezone(&Kolkata).naive_local();
            if !in_market_hours(now_ist) {
                continue;
            }
            let stored = tf_boost_movement_service::record_now(&ctx).await;
            if stored > 0 {
                tracing::debug!("TF Boost recorder stored {stored} rows");
            }
        }
    });
}

/// Sleep until five seconds past the next minute, so a tick reads a ranked list
/// TradeFinder has settled for the minute just closed.
async fn sleep_to_next_minute() {
    let now = chrono::Utc::now();
    let secs = now.second() as u64;
    let wait = 60u64.saturating_sub(secs) + 5;
    tokio::time::sleep(Duration::from_secs(wait)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(hh, mm, 0).unwrap()
    }

    #[test]
    fn records_on_a_weekday_inside_hours() {
        // 2026-01-05 is a Monday.
        assert!(in_market_hours(at(2026, 1, 5, 9, 15)));
        assert!(in_market_hours(at(2026, 1, 5, 12, 30)));
        assert!(in_market_hours(at(2026, 1, 5, 15, 40)));
    }

    #[test]
    fn skips_before_open_and_after_close() {
        assert!(!in_market_hours(at(2026, 1, 5, 9, 14)));
        assert!(!in_market_hours(at(2026, 1, 5, 15, 41)));
        assert!(!in_market_hours(at(2026, 1, 5, 3, 0)));
    }

    #[test]
    fn skips_the_weekend() {
        // 2026-01-10 Saturday, 2026-01-11 Sunday.
        assert!(!in_market_hours(at(2026, 1, 10, 12, 0)));
        assert!(!in_market_hours(at(2026, 1, 11, 12, 0)));
    }
}
