//! Bounded counters shared by database collectors: minute buckets and recent five-second buckets.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{QueryKey, TimeBucket, Timeline, TimelineBasis};

pub const RETAIN_MINUTES: u64 = 7 * 24 * 60;
const MAX_TRACKED_QUERY_MINUTES: usize = 200_000;
const RECENT_RETAIN_BUCKETS: u64 = 120;
const MAX_RECENT_QUERY_ENTRIES: usize = 20_000;

#[derive(Clone, Copy, Debug)]
pub struct MinuteRange {
    pub start: u64,
    pub end: u64,
    recent_end: Option<u64>,
    recent_buckets: u64,
}

impl MinuteRange {
    pub fn recent(minutes: u64) -> Self {
        let end = current_epoch_minute();
        Self {
            start: end.saturating_sub(minutes.clamp(1, RETAIN_MINUTES) - 1),
            end,
            recent_end: (minutes <= 10).then(current_epoch_five_seconds),
            recent_buckets: minutes.clamp(1, 10) * 12,
        }
    }

    pub fn between(start: u64, end: u64) -> Self {
        Self {
            start,
            end,
            recent_end: None,
            recent_buckets: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QueryMetric {
    pub calls: u64,
    pub total_time_ms: f64,
}

#[derive(Debug, Default)]
pub struct TimelineStore {
    totals: HashMap<u64, u64>,
    by_query: HashMap<u64, HashMap<String, QueryMetric>>,
    tracked_entries: usize,
    last_pruned_minute: u64,
    recent_totals: HashMap<u64, u64>,
    recent_by_query: HashMap<u64, HashMap<String, QueryMetric>>,
    recent_tracked_entries: usize,
    last_pruned_recent: u64,
}

pub fn current_epoch_five_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 5
}

pub fn current_epoch_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60
}

impl TimelineStore {
    fn prune_recent(&mut self) {
        let now = current_epoch_five_seconds();
        if self.last_pruned_recent == now {
            return;
        }
        self.last_pruned_recent = now;
        let oldest = now.saturating_sub(RECENT_RETAIN_BUCKETS - 1);
        self.recent_totals.retain(|bucket, _| *bucket >= oldest);
        self.recent_by_query.retain(|bucket, _| *bucket >= oldest);
        self.recent_tracked_entries = self.recent_by_query.values().map(HashMap::len).sum();
    }

    pub fn record_recent(&mut self, bucket: u64, query_key: &str, calls: u64, total_time_ms: f64) {
        if calls == 0
            || bucket < current_epoch_five_seconds().saturating_sub(RECENT_RETAIN_BUCKETS - 1)
        {
            return;
        }
        self.prune_recent();
        *self.recent_totals.entry(bucket).or_default() += calls;
        self.record_recent_scoped(bucket, query_key, calls, total_time_ms);
    }

    pub fn record_recent_scoped(&mut self, bucket: u64, key: &str, calls: u64, total_time_ms: f64) {
        if calls == 0
            || bucket < current_epoch_five_seconds().saturating_sub(RECENT_RETAIN_BUCKETS - 1)
        {
            return;
        }
        self.prune_recent();
        let entries = self.recent_by_query.entry(bucket).or_default();
        if let Some(metric) = entries.get_mut(key) {
            metric.calls += calls;
            metric.total_time_ms += total_time_ms;
        } else if self.recent_tracked_entries < MAX_RECENT_QUERY_ENTRIES {
            entries.insert(
                key.to_owned(),
                QueryMetric {
                    calls,
                    total_time_ms,
                },
            );
            self.recent_tracked_entries += 1;
        }
    }

    fn prune(&mut self) {
        let now = current_epoch_minute();
        if self.last_pruned_minute == now {
            return;
        }
        self.last_pruned_minute = now;
        let oldest = now.saturating_sub(RETAIN_MINUTES - 1);
        self.totals.retain(|minute, _| *minute >= oldest);
        self.by_query.retain(|minute, _| *minute >= oldest);
        self.tracked_entries = self.by_query.values().map(HashMap::len).sum();
    }

    fn record_query(&mut self, minute: u64, key: &str, calls: u64, total_time_ms: f64) {
        let entries = self.by_query.entry(minute).or_default();
        if let Some(metric) = entries.get_mut(key) {
            metric.calls += calls;
            metric.total_time_ms += total_time_ms;
        } else if self.tracked_entries < MAX_TRACKED_QUERY_MINUTES {
            entries.insert(
                key.to_owned(),
                QueryMetric {
                    calls,
                    total_time_ms,
                },
            );
            self.tracked_entries += 1;
        }
    }

    pub fn record(&mut self, minute: u64, query_key: &str, calls: u64, total_time_ms: f64) {
        if calls == 0 || minute < current_epoch_minute().saturating_sub(RETAIN_MINUTES - 1) {
            return;
        }
        self.prune();
        *self.totals.entry(minute).or_default() += calls;
        self.record_query(minute, query_key, calls, total_time_ms);
    }

    pub fn record_scoped(&mut self, minute: u64, key: &str, calls: u64) {
        if calls == 0 || minute < current_epoch_minute().saturating_sub(RETAIN_MINUTES - 1) {
            return;
        }
        self.prune();
        self.record_query(minute, key, calls, 0.0);
    }

    pub fn query_metric(&self, range: MinuteRange, key: &str) -> QueryMetric {
        let mut total = QueryMetric::default();
        if let Some(end) = range.recent_end {
            for bucket in end.saturating_sub(range.recent_buckets - 1)..=end {
                if let Some(value) = self
                    .recent_by_query
                    .get(&bucket)
                    .and_then(|queries| queries.get(key))
                {
                    total.calls += value.calls;
                    total.total_time_ms += value.total_time_ms;
                }
            }
            return total;
        }
        for minute in range.start..=range.end {
            if let Some(value) = self
                .by_query
                .get(&minute)
                .and_then(|queries| queries.get(key))
            {
                total.calls += value.calls;
                total.total_time_ms += value.total_time_ms;
            }
        }
        total
    }

    pub fn query_calls(&self, range: MinuteRange, key: &str) -> u64 {
        self.query_metric(range, key).calls
    }

    pub fn series(
        &self,
        basis: TimelineBasis,
        query_key: Option<&QueryKey>,
        hours: u32,
    ) -> Timeline {
        self.series_range(basis, query_key, MinuteRange::recent(u64::from(hours) * 60))
    }

    pub fn series_range(
        &self,
        basis: TimelineBasis,
        query_key: Option<&QueryKey>,
        range: MinuteRange,
    ) -> Timeline {
        if let Some(end) = range.recent_end {
            let step = match range.recent_buckets {
                0..=12 => 1,
                13..=60 => 6,
                _ => 12,
            };
            let start = end.saturating_sub(range.recent_buckets - 1);
            let buckets = (start..=end)
                .step_by(step as usize)
                .map(|bucket| {
                    let calls = (bucket..=bucket.saturating_add(step - 1).min(end))
                        .map(|part| match query_key {
                            Some(key) => self
                                .recent_by_query
                                .get(&part)
                                .and_then(|queries| queries.get(&key.0))
                                .map_or(0, |metric| metric.calls),
                            None => self.recent_totals.get(&part).copied().unwrap_or(0),
                        })
                        .sum();
                    TimeBucket {
                        start_epoch_seconds: bucket * 5,
                        calls,
                    }
                })
                .collect();
            return Timeline {
                basis,
                bucket_seconds: step * 5,
                buckets,
            };
        }
        let length = range.end - range.start + 1;
        let step = match length {
            0..=60 => 1,
            61..=180 => 5,
            181..=1_440 => 30,
            1_441..=4_320 => 60,
            _ => 180,
        };
        let mut buckets = Vec::new();
        for start in (range.start..=range.end).step_by(step as usize) {
            let end = start.saturating_add(step - 1).min(range.end);
            let calls = (start..=end)
                .map(|minute| match query_key {
                    Some(key) => self
                        .by_query
                        .get(&minute)
                        .and_then(|queries| queries.get(&key.0))
                        .map_or(0, |metric| metric.calls),
                    None => self.totals.get(&minute).copied().unwrap_or(0),
                })
                .sum();
            buckets.push(TimeBucket {
                start_epoch_seconds: start * 60,
                calls,
            });
        }
        Timeline {
            basis,
            bucket_seconds: step * 60,
            buckets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minute_range_excludes_older_calls_and_keeps_duration() {
        let now = current_epoch_minute();
        let mut store = TimelineStore::default();
        store.record(now - 5, "a", 2, 2.5);
        store.record(now - 4, "a", 3, 4.0);
        store.record(now, "b", 1, 1.0);
        let range = MinuteRange::between(now - 4, now);
        assert_eq!(store.query_metric(range, "a").calls, 3);
        assert_eq!(store.query_metric(range, "a").total_time_ms, 4.0);
        let series = store.series_range(TimelineBasis::ObservedByCollector, None, range);
        assert_eq!(series.bucket_seconds, 60);
        assert_eq!(
            series
                .buckets
                .iter()
                .map(|bucket| bucket.calls)
                .sum::<u64>(),
            4
        );
    }

    #[test]
    fn seven_day_window_is_bounded_to_friendly_chart_size() {
        let store = TimelineStore::default();
        let series = store.series_range(
            TimelineBasis::ObservedByCollector,
            None,
            MinuteRange::recent(RETAIN_MINUTES),
        );
        assert_eq!(series.bucket_seconds, 3 * 60 * 60);
        assert_eq!(series.buckets.len(), 56);
    }

    #[test]
    fn one_minute_window_uses_five_second_buckets() {
        let now = current_epoch_five_seconds();
        let mut store = TimelineStore::default();
        store.record_recent(now - 12, "a", 2, 2.0);
        store.record_recent(now - 11, "a", 3, 4.0);
        store.record_recent(now, "b", 1, 1.0);
        let range = MinuteRange::recent(1);
        assert_eq!(store.query_metric(range, "a").calls, 3);
        assert_eq!(store.query_metric(range, "a").total_time_ms, 4.0);
        let series = store.series_range(TimelineBasis::ObservedByCollector, None, range);
        assert_eq!(series.bucket_seconds, 5);
        assert_eq!(series.buckets.len(), 12);
        assert_eq!(
            series
                .buckets
                .iter()
                .map(|bucket| bucket.calls)
                .sum::<u64>(),
            4
        );
    }

    #[test]
    fn five_minute_window_keeps_calls_across_minute_boundary() {
        let now = current_epoch_five_seconds();
        let mut store = TimelineStore::default();
        store.record_recent(now - 60, "a", 2, 2.0);
        store.record_recent(now - 59, "a", 3, 4.0);
        let range = MinuteRange::recent(5);
        assert_eq!(store.query_metric(range, "a").calls, 3);
        assert_eq!(store.query_metric(range, "a").total_time_ms, 4.0);
        let series = store.series_range(TimelineBasis::ObservedByCollector, None, range);
        assert_eq!(series.bucket_seconds, 30);
        assert_eq!(
            series
                .buckets
                .iter()
                .map(|bucket| bucket.calls)
                .sum::<u64>(),
            3
        );
    }
}
