//! Incremental collection from `mysql.general_log` when TABLE output is enabled.
//!
//! The table has no unique row identifier. Re-reading the latest microsecond and
//! comparing row multiplicities prevents duplicate counts across ordinary polls.
//! If more than `max_batch_rows` rows accumulate between polls, collection stops
//! with an error instead of silently dropping a partial batch.

use std::collections::{HashMap, VecDeque};

use sqlx::{MySqlPool, Row};
use tokio::sync::Mutex;

use crate::backend::{BackendError, BackendErrorKind};
use crate::model::{
    HostFilter, HostStats, QueryFilter, QueryKey, QuerySort, QuerySource, QueryStats,
    RecentStatement, Timeline, TimelineBasis,
};
use crate::timeline::{MinuteRange, TimelineStore};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LogIdentity {
    thread_id: u64,
    command_type: String,
    argument: String,
    argument_hash: String,
    user_host: String,
}

#[derive(Clone, Debug)]
struct LogRow {
    event_time: String,
    epoch_minute: u64,
    epoch_five_seconds: u64,
    identity: LogIdentity,
    truncated: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct StatementKey {
    host: String,
    sql_hash: String,
    sql: String,
}

#[derive(Debug)]
struct CollectorState {
    watermark: String,
    seen_at_watermark: HashMap<LogIdentity, u32>,
    own_threads: Vec<u64>,
    ignored_username: String,
    statements: HashMap<StatementKey, u64>,
    hosts: HashMap<String, u64>,
    recent: VecDeque<RecentStatement>,
    timeline: TimelineStore,
    max_queries: usize,
    max_recent: usize,
    dropped_new_queries: u64,
}

impl CollectorState {
    fn ingest(&mut self, rows: Vec<LogRow>) -> usize {
        let mut timestamp = self.watermark.clone();
        let mut seen_this_poll: HashMap<LogIdentity, u32> = HashMap::new();
        let mut added = 0;

        for row in rows {
            if row.event_time != timestamp {
                timestamp = row.event_time.clone();
                seen_this_poll.clear();
            }
            let occurrence = seen_this_poll
                .entry(row.identity.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            if row.event_time == self.watermark
                && *occurrence
                    <= self
                        .seen_at_watermark
                        .get(&row.identity)
                        .copied()
                        .unwrap_or(0)
            {
                continue;
            }
            if self.own_threads.contains(&row.identity.thread_id)
                || row
                    .identity
                    .user_host
                    .starts_with(&format!("{}[", self.ignored_username))
                || !matches!(row.identity.command_type.as_str(), "Query" | "Execute")
            {
                continue;
            }

            let host = extract_host(&row.identity.user_host);
            self.timeline
                .record(row.epoch_minute, &row.identity.argument_hash, 1, 0.0);
            self.timeline.record_recent(
                row.epoch_five_seconds,
                &row.identity.argument_hash,
                1,
                0.0,
            );
            self.timeline.record_scoped(
                row.epoch_minute,
                &format!("{host}:{}", row.identity.argument_hash),
                1,
            );
            self.timeline.record_recent_scoped(
                row.epoch_five_seconds,
                &format!("{host}:{}", row.identity.argument_hash),
                1,
                0.0,
            );
            let mut sql = redact_admin_statement(row.identity.argument);
            if row.truncated && sql != "[administrative statement redacted]" {
                sql.push_str("\n…[truncated]");
            }
            let key = StatementKey {
                host: host.clone(),
                sql_hash: row.identity.argument_hash,
                sql: sql.clone(),
            };
            if let Some(count) = self.statements.get_mut(&key) {
                *count += 1;
            } else if self.statements.len() < self.max_queries {
                self.statements.insert(key, 1);
            } else {
                self.dropped_new_queries += 1;
            }
            *self.hosts.entry(host.clone()).or_default() += 1;
            self.recent.push_back(RecentStatement {
                source: QuerySource::MySqlGeneralLog,
                query_key: None,
                namespace: None,
                sql,
                host: Some(host),
                duration_ms: None,
            });
            while self.recent.len() > self.max_recent {
                self.recent.pop_front();
            }
            added += 1;
        }

        if timestamp != self.watermark || !seen_this_poll.is_empty() {
            self.watermark = timestamp;
            self.seen_at_watermark = seen_this_poll;
        }
        added
    }

    fn query_stats(
        &self,
        filter: &QueryFilter,
        sort: QuerySort,
    ) -> Result<Vec<QueryStats>, BackendError> {
        if filter.namespace.is_some() {
            return Err(BackendError {
                kind: BackendErrorKind::Unsupported,
                message: "general query log에는 쿼리의 기본 스키마 정보가 없습니다".into(),
            });
        }
        if sort != QuerySort::Calls {
            return Err(BackendError {
                kind: BackendErrorKind::Unsupported,
                message: "general query log에는 실행 시간과 검사 행 수가 없습니다".into(),
            });
        }
        let mut totals: HashMap<&str, (&str, u64)> = HashMap::new();
        for (key, calls) in &self.statements {
            if filter.host.as_ref().is_some_and(|host| host != &key.host)
                || filter
                    .text_contains
                    .as_ref()
                    .is_some_and(|text| !key.sql.contains(text))
            {
                continue;
            }
            let entry = totals.entry(&key.sql_hash).or_insert((&key.sql, 0));
            entry.1 += calls;
        }
        let mut rows: Vec<_> = totals
            .into_iter()
            .map(|(hash, (sql, calls))| QueryStats {
                source: QuerySource::MySqlGeneralLog,
                key: QueryKey(hash.to_owned()),
                namespace: None,
                statement_text: sql.to_owned(),
                calls,
                total_time_ms: None,
                mean_time_ms: None,
                rows_examined: None,
                no_index_used: None,
                no_good_index_used: None,
            })
            .collect();
        rows.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.key.0.cmp(&b.key.0)));
        Ok(rows
            .into_iter()
            .skip(filter.offset as usize)
            .take(filter.limit as usize)
            .collect())
    }

    fn query_stats_in_range(
        &self,
        filter: &QueryFilter,
        sort: QuerySort,
        range: MinuteRange,
    ) -> Result<Vec<QueryStats>, BackendError> {
        if sort != QuerySort::Calls {
            return Err(BackendError {
                kind: BackendErrorKind::Unsupported,
                message: "시간 구간에는 호출 수 정렬만 지원합니다".into(),
            });
        }
        let mut all = filter.clone();
        all.limit = 5_000;
        all.offset = 0;
        let mut rows = self.query_stats(&all, sort)?;
        for row in &mut rows {
            let key = match &filter.host {
                Some(host) => format!("{host}:{}", row.key.0),
                None => row.key.0.clone(),
            };
            row.calls = self.timeline.query_calls(range, &key);
        }
        rows.retain(|row| row.calls > 0);
        rows.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.key.0.cmp(&b.key.0)));
        Ok(rows
            .into_iter()
            .skip(filter.offset as usize)
            .take(filter.limit as usize)
            .collect())
    }

    fn host_stats(&self, filter: &HostFilter) -> Vec<HostStats> {
        let mut rows: Vec<_> = self
            .hosts
            .iter()
            .filter(|(host, _)| {
                filter
                    .text_contains
                    .as_ref()
                    .is_none_or(|text| host.contains(text))
            })
            .map(|(host, calls)| HostStats {
                host: host.clone(),
                current_connections: None,
                total_connections: None,
                statement_calls: Some(*calls),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.statement_calls
                .cmp(&a.statement_calls)
                .then_with(|| a.host.cmp(&b.host))
        });
        rows.into_iter()
            .skip(filter.offset as usize)
            .take(filter.limit as usize)
            .collect()
    }
}

fn extract_host(user_host: &str) -> String {
    if let Some(ip) = user_host
        .rsplit_once('[')
        .and_then(|(_, host)| host.strip_suffix(']'))
        .filter(|host| !host.is_empty())
    {
        return ip.to_owned();
    }
    user_host
        .split_once('@')
        .map(|(_, host)| host.split('[').next().unwrap_or(host).trim())
        .filter(|host| !host.is_empty())
        .unwrap_or(user_host)
        .to_owned()
}

fn redact_admin_statement(sql: String) -> String {
    let upper = sql.trim_start().to_ascii_uppercase();
    if [
        "CREATE USER",
        "ALTER USER",
        "CREATE ROLE",
        "ALTER ROLE",
        "SET PASSWORD",
        "GRANT ",
        "REVOKE ",
        "SET ",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
    {
        "[administrative statement redacted]".into()
    } else {
        sql
    }
}

fn query_error(error: sqlx::Error) -> BackendError {
    BackendError {
        kind: BackendErrorKind::Query,
        message: format!("MySQL general log 조회 실패: {error}"),
    }
}

pub struct GeneralLogCollector {
    pool: MySqlPool,
    state: Mutex<CollectorState>,
    max_batch_rows: u32,
}

impl GeneralLogCollector {
    /// Starts at the current server time. Old log rows are intentionally ignored.
    pub async fn new(
        pool: MySqlPool,
        max_batch_rows: u32,
        max_queries: usize,
        max_recent: usize,
    ) -> Result<Self, BackendError> {
        if max_batch_rows == 0 || max_queries == 0 {
            return Err(BackendError {
                kind: BackendErrorKind::InvalidRequest,
                message: "수집 한도는 0보다 커야 합니다".into(),
            });
        }
        let mut connection = pool.acquire().await.map_err(query_error)?;
        sqlx::query("SET time_zone = '+00:00'")
            .execute(&mut *connection)
            .await
            .map_err(query_error)?;
        let (watermark, ignored_username): (String, String) = sqlx::query_as(
            "SELECT DATE_FORMAT(UTC_TIMESTAMP(6), '%Y-%m-%d %H:%i:%s.%f'), SUBSTRING_INDEX(USER(), '@', 1)",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(query_error)?;
        Ok(Self {
            pool,
            state: Mutex::new(CollectorState {
                watermark,
                seen_at_watermark: HashMap::new(),
                own_threads: Vec::new(),
                ignored_username,
                statements: HashMap::new(),
                hosts: HashMap::new(),
                recent: VecDeque::new(),
                timeline: TimelineStore::default(),
                max_queries,
                max_recent,
                dropped_new_queries: 0,
            }),
            max_batch_rows,
        })
    }

    pub async fn is_available(&self) -> Result<bool, BackendError> {
        let (enabled, output): (String, String) = sqlx::query_as(
            "SELECT CAST(@@GLOBAL.general_log AS CHAR), CAST(@@GLOBAL.log_output AS CHAR)",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(query_error)?;
        Ok(enabled == "1" && output.split(',').any(|part| part.trim() == "TABLE"))
    }

    /// Polls once. The caller controls interval and backoff.
    pub async fn poll(&self) -> Result<usize, BackendError> {
        let mut state = self.state.lock().await;
        let mut connection = self.pool.acquire().await.map_err(query_error)?;
        sqlx::query("SET time_zone = '+00:00'")
            .execute(&mut *connection)
            .await
            .map_err(query_error)?;
        let (thread_id,): (u64,) = sqlx::query_as("SELECT CONNECTION_ID()")
            .fetch_one(&mut *connection)
            .await
            .map_err(query_error)?;
        if !state.own_threads.contains(&thread_id) {
            state.own_threads.push(thread_id);
        }
        let (enabled, output): (String, String) = sqlx::query_as(
            "SELECT CAST(@@GLOBAL.general_log AS CHAR), CAST(@@GLOBAL.log_output AS CHAR)",
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(query_error)?;
        if enabled != "1" || !output.split(',').any(|part| part.trim() == "TABLE") {
            return Err(BackendError {
                kind: BackendErrorKind::Unsupported,
                message: "general_log=ON 및 log_output=TABLE 설정이 필요합니다".into(),
            });
        }
        let rows = sqlx::query(
            "SELECT DATE_FORMAT(event_time, '%Y-%m-%d %H:%i:%s.%f') AS event_time, \
             CAST(FLOOR(UNIX_TIMESTAMP(event_time) / 60) AS UNSIGNED) AS epoch_minute, \
             CAST(FLOOR(UNIX_TIMESTAMP(event_time) / 5) AS UNSIGNED) AS epoch_five_seconds, \
             thread_id, command_type, LEFT(CAST(argument AS CHAR), 8192) AS argument, \
             SHA2(argument, 256) AS argument_hash, \
             CAST(LENGTH(argument) > 8192 AS UNSIGNED) AS truncated, user_host \
             FROM mysql.general_log WHERE event_time >= ? \
             ORDER BY event_time, thread_id LIMIT ?",
        )
        .bind(&state.watermark)
        .bind(self.max_batch_rows + 1)
        .fetch_all(&mut *connection)
        .await
        .map_err(query_error)?;
        if rows.len() > self.max_batch_rows as usize {
            return Err(BackendError {
                kind: BackendErrorKind::Query,
                message: "general log 한 번의 수집 한도를 초과했습니다. 수집 주기를 줄이거나 한도를 늘리세요".into(),
            });
        }
        let parsed = rows
            .into_iter()
            .map(|row| {
                Ok(LogRow {
                    event_time: row.try_get("event_time").map_err(query_error)?,
                    epoch_minute: row.try_get("epoch_minute").map_err(query_error)?,
                    epoch_five_seconds: row.try_get("epoch_five_seconds").map_err(query_error)?,
                    identity: LogIdentity {
                        thread_id: row.try_get("thread_id").map_err(query_error)?,
                        command_type: row.try_get("command_type").map_err(query_error)?,
                        argument: row.try_get("argument").map_err(query_error)?,
                        argument_hash: row.try_get("argument_hash").map_err(query_error)?,
                        user_host: row.try_get("user_host").map_err(query_error)?,
                    },
                    truncated: row.try_get::<u64, _>("truncated").map_err(query_error)? != 0,
                })
            })
            .collect::<Result<Vec<_>, BackendError>>()?;
        Ok(state.ingest(parsed))
    }

    pub async fn query_stats(
        &self,
        filter: &QueryFilter,
        sort: QuerySort,
    ) -> Result<Vec<QueryStats>, BackendError> {
        self.state.lock().await.query_stats(filter, sort)
    }

    pub async fn query_stats_in_range(
        &self,
        filter: &QueryFilter,
        sort: QuerySort,
        range: MinuteRange,
    ) -> Result<Vec<QueryStats>, BackendError> {
        self.state
            .lock()
            .await
            .query_stats_in_range(filter, sort, range)
    }

    pub async fn host_stats(&self, filter: &HostFilter) -> Vec<HostStats> {
        self.state.lock().await.host_stats(filter)
    }

    pub async fn recent_statements(&self, limit: u32) -> Vec<RecentStatement> {
        self.state
            .lock()
            .await
            .recent
            .iter()
            .rev()
            .take(limit as usize)
            .cloned()
            .collect()
    }

    pub async fn dropped_new_queries(&self) -> u64 {
        self.state.lock().await.dropped_new_queries
    }

    pub async fn timeline(&self, query_key: Option<&QueryKey>, hours: u32) -> Timeline {
        self.state
            .lock()
            .await
            .timeline
            .series(TimelineBasis::ReceivedAtServer, query_key, hours)
    }

    pub async fn timeline_range(
        &self,
        query_key: Option<&QueryKey>,
        range: MinuteRange,
    ) -> Timeline {
        self.state.lock().await.timeline.series_range(
            TimelineBasis::ReceivedAtServer,
            query_key,
            range,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn row(time: &str, sql: &str) -> LogRow {
        LogRow {
            event_time: time.into(),
            epoch_minute: crate::timeline::current_epoch_minute(),
            epoch_five_seconds: crate::timeline::current_epoch_five_seconds(),
            identity: LogIdentity {
                thread_id: 12,
                command_type: "Query".into(),
                argument: sql.into(),
                argument_hash: format!("{:x}", Sha256::digest(sql.as_bytes())),
                user_host: "user[user] @ app [10.0.0.1]".into(),
            },
            truncated: false,
        }
    }

    #[test]
    fn same_timestamp_rows_are_counted_once_across_polls() {
        let mut state = CollectorState {
            watermark: "2026-10-07 00:00:00.000001".into(),
            seen_at_watermark: HashMap::new(),
            own_threads: vec![],
            ignored_username: "jansori".into(),
            statements: HashMap::new(),
            hosts: HashMap::new(),
            recent: VecDeque::new(),
            timeline: TimelineStore::default(),
            max_queries: 10,
            max_recent: 10,
            dropped_new_queries: 0,
        };
        let one = row("2026-10-07 00:00:00.000001", "SELECT 1");
        assert_eq!(state.ingest(vec![one.clone()]), 1);
        assert_eq!(state.ingest(vec![one.clone()]), 0);
        assert_eq!(state.ingest(vec![one.clone(), one]), 1);
        assert_eq!(state.hosts["10.0.0.1"], 2);
        let hour = crate::timeline::current_epoch_minute();
        let filter = QueryFilter {
            host: Some("10.0.0.1".into()),
            limit: 10,
            ..QueryFilter::default()
        };
        let rows = state
            .query_stats_in_range(&filter, QuerySort::Calls, MinuteRange::between(hour, hour))
            .unwrap();
        assert_eq!(rows[0].calls, 2);
    }

    #[test]
    fn non_statements_and_own_queries_are_excluded() {
        let mut state = CollectorState {
            watermark: "2026-10-07 00:00:00.000001".into(),
            seen_at_watermark: HashMap::new(),
            own_threads: vec![99],
            ignored_username: "jansori".into(),
            statements: HashMap::new(),
            hosts: HashMap::new(),
            recent: VecDeque::new(),
            timeline: TimelineStore::default(),
            max_queries: 10,
            max_recent: 10,
            dropped_new_queries: 0,
        };
        let mut own = row("2026-10-07 00:00:00.000002", "SELECT 1");
        own.identity.thread_id = 99;
        let mut connect = row("2026-10-07 00:00:00.000003", "Connect");
        connect.identity.command_type = "Connect".into();
        assert_eq!(state.ingest(vec![own, connect]), 0);
    }

    #[test]
    fn administrative_passwords_are_not_exposed() {
        assert_eq!(
            redact_admin_statement("CREATE USER x IDENTIFIED BY 'secret'".into()),
            "[administrative statement redacted]"
        );
        assert_eq!(redact_admin_statement("SELECT 1".into()), "SELECT 1");
    }
}
