use time::{Duration, OffsetDateTime, Time};

/// The soonest future occurrence of any of `times` (each `"HH:MM"`), relative to
/// `now` (in `now`'s offset). Today's earliest still-future time, else tomorrow's
/// earliest. `None` if `times` is empty or none parse.
pub fn next_fire(times: &[String], now: OffsetDateTime) -> Option<OffsetDateTime> {
    times
        .iter()
        .filter_map(|hhmm| {
            let (h, m) = hhmm.split_once(':')?;
            let t = Time::from_hms(h.trim().parse().ok()?, m.trim().parse().ok()?, 0).ok()?;
            let mut cand = now.replace_time(t);
            if cand <= now {
                cand += Duration::days(1);
            }
            Some(cand)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed "now": 2026-06-09 12:00:00 UTC.
    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_781_006_400).unwrap()
    }

    #[test]
    fn picks_earliest_future_time_today() {
        let nf = next_fire(&["06:00".into(), "16:00".into(), "21:00".into()], now()).unwrap();
        assert_eq!(nf.hour(), 16);
        assert_eq!(nf.minute(), 0);
        assert_eq!(nf.date(), now().date());
    }

    #[test]
    fn rolls_to_tomorrow_when_all_past() {
        let nf = next_fire(&["06:00".into(), "09:00".into()], now()).unwrap();
        assert_eq!(nf.hour(), 6);
        assert_eq!(nf.date(), now().date() + Duration::days(1));
    }

    #[test]
    fn empty_or_unparsable_is_none() {
        assert!(next_fire(&[], now()).is_none());
        assert!(next_fire(&["nope".into()], now()).is_none());
    }
}
