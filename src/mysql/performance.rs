//! Time-aligned MySQL statement digest deltas for the bottleneck view.

use std::collections::{HashMap, HashSet};

use sqlx::MySqlPool;
use tokio::sync::Mutex;

use crate::backend::{BackendError, BackendErrorKind};
use crate::model::BottleneckStats;
use crate::timeline::{
    current_epoch_five_seconds, current_epoch_minute, MinuteRange, RETAIN_MINUTES,
};

const MAX_BUCKETS: usize = 200_000;
const MAX_RECENT_BUCKETS: usize = 20_000;

#[derive(Clone, Copy, Default)]
struct Counters {
    calls: u64,
    time_ps: u64,
    examined: u64,
    sent: u64,
}

impl Counters {
    fn delta(self, previous: Self) -> Self {
        // A reset never turns old server totals into newly observed work.
        if self.calls < previous.calls {
            return Self::default();
        }
        Self {
            calls: self.calls - previous.calls,
            time_ps: self.time_ps.saturating_sub(previous.time_ps),
            examined: self.examined.saturating_sub(previous.examined),
            sent: self.sent.saturating_sub(previous.sent),
        }
    }

    fn add(&mut self, other: Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.time_ps = self.time_ps.saturating_add(other.time_ps);
        self.examined = self.examined.saturating_add(other.examined);
        self.sent = self.sent.saturating_add(other.sent);
    }
}

#[derive(Clone)]
struct DigestRow {
    namespace: String,
    digest_text: String,
    sample_sql: Option<String>,
}

#[derive(Default)]
struct State {
    initialized: bool,
    last_error: Option<String>,
    previous: HashMap<String, Counters>,
    digests: HashMap<String, DigestRow>,
    totals: HashMap<String, Counters>,
    minutes: HashMap<u64, HashMap<String, Counters>>,
    recent: HashMap<u64, HashMap<String, Counters>>,
}

impl State {
    fn ingest(&mut self, rows: Vec<(String, String, String, Option<String>, u64, u64, u64, u64)>) {
        let minute = current_epoch_minute();
        let five_seconds = current_epoch_five_seconds();
        self.minutes
            .retain(|bucket, _| *bucket >= minute.saturating_sub(RETAIN_MINUTES - 1));
        self.recent
            .retain(|bucket, _| *bucket >= five_seconds.saturating_sub(119));
        let mut tracked_minutes: usize = self.minutes.values().map(HashMap::len).sum();
        let mut tracked_recent: usize = self.recent.values().map(HashMap::len).sum();
        for (namespace, digest, digest_text, sample_sql, calls, time_ps, examined, sent) in rows {
            // Monitoring queries must not appear as application bottlenecks.
            if digest_text.contains("performance_schema")
                || digest_text.contains("mysql.general_log")
            {
                continue;
            }
            let key = format!("{namespace}:{digest}");
            let current = Counters {
                calls,
                time_ps,
                examined,
                sent,
            };
            let delta = if self.initialized {
                self.previous
                    .get(&key)
                    .map_or(current, |previous| current.delta(*previous))
            } else {
                Counters::default()
            };
            self.previous.insert(key.clone(), current);
            self.digests.insert(
                key.clone(),
                DigestRow {
                    namespace,
                    digest_text,
                    sample_sql,
                },
            );
            if delta.calls == 0 {
                continue;
            }
            self.totals.entry(key.clone()).or_default().add(delta);
            let minute_rows = self.minutes.entry(minute).or_default();
            if minute_rows.contains_key(&key) || tracked_minutes < MAX_BUCKETS {
                if !minute_rows.contains_key(&key) {
                    tracked_minutes += 1;
                }
                minute_rows.entry(key.clone()).or_default().add(delta);
            }
            let recent_rows = self.recent.entry(five_seconds).or_default();
            if recent_rows.contains_key(&key) || tracked_recent < MAX_RECENT_BUCKETS {
                if !recent_rows.contains_key(&key) {
                    tracked_recent += 1;
                }
                recent_rows.entry(key).or_default().add(delta);
            }
        }
        self.initialized = true;
    }

    fn list(
        &self,
        namespace: Option<&str>,
        range: Option<MinuteRange>,
        limit: usize,
    ) -> Vec<BottleneckStats> {
        let mut totals: HashMap<String, Counters> = HashMap::new();
        match range {
            None => totals.extend(self.totals.iter().map(|(key, value)| (key.clone(), *value))),
            Some(range) => {
                if let Some((start, end)) = range.recent_five_second_bounds() {
                    for bucket in start..=end {
                        if let Some(rows) = self.recent.get(&bucket) {
                            for (key, value) in rows {
                                totals.entry(key.clone()).or_default().add(*value);
                            }
                        }
                    }
                } else {
                    for bucket in range.start..=range.end {
                        if let Some(rows) = self.minutes.get(&bucket) {
                            for (key, value) in rows {
                                totals.entry(key.clone()).or_default().add(*value);
                            }
                        }
                    }
                }
            }
        }
        let mut result: Vec<_> = totals
            .into_iter()
            .filter_map(|(key, value)| {
                let digest = self.digests.get(&key)?;
                if value.calls == 0 || namespace.is_some_and(|name| name != digest.namespace) {
                    return None;
                }
                let total_time_ms = value.time_ps as f64 / 1_000_000_000.0;
                Some(BottleneckStats {
                    key,
                    namespace: digest.namespace.clone(),
                    statement_text: digest.digest_text.clone(),
                    calls: value.calls,
                    total_time_ms,
                    mean_time_ms: total_time_ms / value.calls as f64,
                    rows_examined: value.examined,
                    rows_sent: value.sent,
                })
            })
            .collect();
        result.sort_by(|a, b| {
            b.total_time_ms
                .total_cmp(&a.total_time_ms)
                .then_with(|| a.key.cmp(&b.key))
        });
        result.truncate(limit);
        result
    }
}

pub struct PerformanceCollector {
    pool: MySqlPool,
    state: Mutex<State>,
    allowed_schemas: HashSet<String>,
}

impl PerformanceCollector {
    pub fn new(pool: MySqlPool, allowed_schemas: HashSet<String>) -> Self {
        Self {
            pool,
            state: Mutex::new(State::default()),
            allowed_schemas,
        }
    }

    pub async fn poll(&self) -> Result<(), BackendError> {
        let result =
            sqlx::query_as::<_, (String, String, String, Option<String>, u64, u64, u64, u64)>(
                "SELECT SCHEMA_NAME, DIGEST, DIGEST_TEXT, QUERY_SAMPLE_TEXT, COUNT_STAR, \
             SUM_TIMER_WAIT, SUM_ROWS_EXAMINED, SUM_ROWS_SENT \
             FROM performance_schema.events_statements_summary_by_digest \
             WHERE SCHEMA_NAME IS NOT NULL AND DIGEST IS NOT NULL LIMIT 10001",
            )
            .fetch_all(&self.pool)
            .await;
        let rows = match result {
            Ok(rows) => rows,
            Err(error) => {
                let message = format!("Performance Schema 조회 실패: {error}");
                self.state.lock().await.last_error = Some(message.clone());
                return Err(BackendError {
                    kind: BackendErrorKind::Query,
                    message,
                });
            }
        };
        if rows.len() > 10_000 {
            let message = "statement digest 수집 한도(10,000개)를 초과했습니다".to_owned();
            self.state.lock().await.last_error = Some(message.clone());
            return Err(BackendError {
                kind: BackendErrorKind::Query,
                message,
            });
        }
        let rows = rows
            .into_iter()
            .filter(|row| self.allowed_schemas.contains(&row.0))
            .collect();
        let mut state = self.state.lock().await;
        state.last_error = None;
        state.ingest(rows);
        Ok(())
    }

    pub async fn list(
        &self,
        namespace: Option<&str>,
        range: Option<MinuteRange>,
        limit: usize,
    ) -> Result<Vec<BottleneckStats>, BackendError> {
        let state = self.state.lock().await;
        if let Some(message) = &state.last_error {
            return Err(BackendError {
                kind: BackendErrorKind::Query,
                message: message.clone(),
            });
        }
        Ok(state.list(namespace, range, limit))
    }

    pub async fn sample_sql(&self, key: &str) -> Option<(String, String)> {
        self.state.lock().await.digests.get(key).and_then(|row| {
            row.sample_sql
                .as_ref()
                .map(|sql| (row.namespace.clone(), sql.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        calls: u64,
        time_ps: u64,
        examined: u64,
    ) -> (String, String, String, Option<String>, u64, u64, u64, u64) {
        (
            "app".into(),
            "abc".into(),
            "SELECT * FROM `posts` WHERE `id` = ?".into(),
            Some("SELECT * FROM posts WHERE id = 1".into()),
            calls,
            time_ps,
            examined,
            calls,
        )
    }

    #[test]
    fn first_poll_is_baseline_and_following_polls_record_only_deltas() {
        let mut state = State::default();
        state.ingest(vec![row(10, 10_000, 100)]);
        assert!(state.list(Some("app"), None, 10).is_empty());
        state.ingest(vec![row(13, 16_000, 145)]);
        state.ingest(vec![row(13, 16_000, 145)]);
        let result = state.list(Some("app"), None, 10);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].calls, 3);
        assert_eq!(result[0].rows_examined, 45);
        assert_eq!(result[0].rows_sent, 3);
        assert_eq!(result[0].total_time_ms, 0.000006);
        assert!(state.list(Some("other"), None, 10).is_empty());
    }
}
