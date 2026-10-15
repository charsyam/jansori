//! Incremental collection from `mysql.general_log` when TABLE output is enabled.
//!
//! The table has no unique row identifier. Re-reading the latest microsecond and
//! comparing row multiplicities prevents duplicate counts across ordinary polls.
//! If more than `max_batch_rows` rows accumulate between polls, collection stops
//! with an error instead of silently dropping a partial batch.

use std::collections::{HashMap, HashSet, VecDeque};

use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, MySqlPool, Row};
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
    namespace: Option<String>,
    host: String,
    sql_hash: String,
    sql: String,
}

#[derive(Debug)]
struct StatementAggregate {
    calls: u64,
    sample_sql: String,
}

#[derive(Debug)]
struct CollectorState {
    watermark: String,
    seen_at_watermark: HashMap<LogIdentity, u32>,
    own_threads: Vec<u64>,
    thread_databases: HashMap<u64, Option<String>>,
    ignored_username: String,
    statements: HashMap<StatementKey, StatementAggregate>,
    hosts: HashMap<String, u64>,
    recent: VecDeque<RecentStatement>,
    timeline: TimelineStore,
    max_queries: usize,
    max_recent: usize,
    dropped_new_queries: u64,
}

impl CollectorState {
    fn update_thread_database(&mut self, thread_id: u64, command: &str, argument: &str) -> bool {
        match command {
            "Connect" | "Change user" => {
                self.thread_databases
                    .insert(thread_id, connect_database(argument));
                true
            }
            "Init DB" => {
                self.thread_databases
                    .insert(thread_id, Some(argument.trim().to_owned()));
                true
            }
            "Quit" => {
                self.thread_databases.remove(&thread_id);
                true
            }
            "Query" | "Execute" => {
                if let Some(database) = use_database(argument) {
                    self.thread_databases.insert(thread_id, Some(database));
                }
                false
            }
            _ => true,
        }
    }

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
            {
                continue;
            }

            let thread_id = row.identity.thread_id;
            if self.update_thread_database(
                thread_id,
                &row.identity.command_type,
                &row.identity.argument,
            ) {
                continue;
            }
            let namespace = self.thread_databases.get(&thread_id).cloned().flatten();

            let host = extract_host(&row.identity.user_host);
            let raw_sql = row.identity.argument;
            let sql = redact_admin_statement(raw_sql.clone());
            let normalized = normalize_statement(&sql);
            // The log only supplies the first 8 KiB. Keep the full raw hash for
            // truncated statements so a different suffix cannot be merged.
            let hash_text = if row.truncated {
                &row.identity.argument_hash
            } else {
                &normalized
            };
            let mut digest = Sha256::new();
            digest.update(namespace.as_deref().unwrap_or(""));
            digest.update([0]);
            digest.update(hash_text);
            let sql_hash = format!("{:x}", digest.finalize());
            self.timeline.record(row.epoch_minute, &sql_hash, 1, 0.0);
            self.timeline
                .record_recent(row.epoch_five_seconds, &sql_hash, 1, 0.0);
            self.timeline
                .record_scoped(row.epoch_minute, &format!("{host}:{sql_hash}"), 1);
            self.timeline.record_recent_scoped(
                row.epoch_five_seconds,
                &format!("{host}:{sql_hash}"),
                1,
                0.0,
            );
            if let Some(database) = &namespace {
                self.timeline
                    .record_scoped(row.epoch_minute, &format!("db:{database}"), 1);
                self.timeline.record_recent_scoped(
                    row.epoch_five_seconds,
                    &format!("db:{database}"),
                    1,
                    0.0,
                );
            }
            let mut display_sql = normalized;
            if row.truncated && sql != "[administrative statement redacted]" {
                display_sql.push_str("\n…[truncated]");
            }
            let key = StatementKey {
                namespace: namespace.clone(),
                host: host.clone(),
                sql_hash,
                sql: display_sql,
            };
            if let Some(aggregate) = self.statements.get_mut(&key) {
                aggregate.calls += 1;
            } else if self.statements.len() < self.max_queries {
                self.statements.insert(
                    key,
                    StatementAggregate {
                        calls: 1,
                        sample_sql: raw_sql,
                    },
                );
            } else {
                self.dropped_new_queries += 1;
            }
            *self.hosts.entry(host.clone()).or_default() += 1;
            self.recent.push_back(RecentStatement {
                source: QuerySource::MySqlGeneralLog,
                query_key: None,
                namespace,
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
        if sort != QuerySort::Calls {
            return Err(BackendError {
                kind: BackendErrorKind::Unsupported,
                message: "general query log에는 실행 시간과 검사 행 수가 없습니다".into(),
            });
        }
        let mut totals: HashMap<&str, (&str, Option<&str>, u64)> = HashMap::new();
        for (key, aggregate) in &self.statements {
            if filter
                .namespace
                .as_ref()
                .is_some_and(|database| key.namespace.as_ref() != Some(database))
                || filter.host.as_ref().is_some_and(|host| host != &key.host)
                || filter
                    .text_contains
                    .as_ref()
                    .is_some_and(|text| !key.sql.contains(text))
            {
                continue;
            }
            let entry =
                totals
                    .entry(&key.sql_hash)
                    .or_insert((&key.sql, key.namespace.as_deref(), 0));
            entry.2 += aggregate.calls;
        }
        let mut rows: Vec<_> = totals
            .into_iter()
            .map(|(hash, (sql, namespace, calls))| QueryStats {
                source: QuerySource::MySqlGeneralLog,
                key: QueryKey(hash.to_owned()),
                namespace: namespace.map(str::to_owned),
                statement_text: sql.to_owned(),
                calls,
                total_time_ms: None,
                mean_time_ms: None,
                max_time_ms: None,
                rows_examined: None,
                rows_sent: None,
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
            let key = filter
                .host
                .as_ref()
                .map_or_else(|| row.key.0.clone(), |host| format!("{host}:{}", row.key.0));
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
        let mut calls_by_host: HashMap<&str, u64> = HashMap::new();
        if let Some(database) = &filter.namespace {
            for (key, aggregate) in &self.statements {
                if key.namespace.as_ref() == Some(database) {
                    *calls_by_host.entry(&key.host).or_default() += aggregate.calls;
                }
            }
        } else {
            calls_by_host.extend(
                self.hosts
                    .iter()
                    .map(|(host, calls)| (host.as_str(), *calls)),
            );
        }
        let mut rows: Vec<_> = calls_by_host
            .into_iter()
            .filter(|(host, _)| {
                filter
                    .text_contains
                    .as_ref()
                    .is_none_or(|text| host.contains(text))
            })
            .map(|(host, calls)| HostStats {
                host: host.to_owned(),
                current_connections: None,
                total_connections: None,
                statement_calls: Some(calls),
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

    fn sample_sql(&self, key: &QueryKey) -> Option<(Option<String>, String)> {
        self.statements
            .iter()
            .find(|(statement, _)| statement.sql_hash == key.0)
            .map(|(statement, aggregate)| {
                (statement.namespace.clone(), aggregate.sample_sql.clone())
            })
    }
}

/// Replaces MySQL literal values while preserving identifiers and SQL operators.
fn normalize_statement(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut normalized = String::with_capacity(sql.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if !normalized.is_empty() && !normalized.ends_with(' ') {
                normalized.push(' ');
            }
            i += 1;
            continue;
        }
        // Double quotes may be identifiers under ANSI_QUOTES, so leave them intact.
        if c == '`' || c == '\'' {
            let quote = c;
            let start = i;
            let mut closed = false;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && quote != '`' && i + 1 < chars.len() {
                    i += 2;
                } else if chars[i] == quote {
                    i += 1;
                    if i < chars.len() && chars[i] == quote {
                        i += 1;
                    } else {
                        closed = true;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            if quote == '`' || !closed {
                normalized.extend(&chars[start..i]);
            } else {
                normalized.push('?');
            }
            continue;
        }
        if c == '#'
            || c == '-'
                && chars.get(i + 1) == Some(&'-')
                && chars.get(i + 2).is_none_or(|next| next.is_whitespace())
        {
            while i < chars.len() && chars[i] != '\n' {
                normalized.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            normalized.push('/');
            normalized.push('*');
            i += 2;
            while i < chars.len() {
                let end = chars[i] == '*' && chars.get(i + 1) == Some(&'/');
                normalized.push(chars[i]);
                i += 1;
                if end {
                    normalized.push('/');
                    i += 1;
                    break;
                }
            }
            continue;
        }
        let numeric_start =
            c.is_ascii_digit() || c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit);
        let previous_is_identifier =
            i > 0 && (chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '_' | '$'));
        if numeric_start && !previous_is_identifier {
            let start = i;
            if c == '0' && matches!(chars.get(i + 1), Some('x' | 'X' | 'b' | 'B')) {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if chars.get(i) == Some(&'.') {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if matches!(chars.get(i), Some('e' | 'E')) {
                    let exponent = i;
                    i += 1;
                    if matches!(chars.get(i), Some('+' | '-')) {
                        i += 1;
                    }
                    let digits = i;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    if digits == i {
                        i = exponent;
                    }
                }
            }
            if chars
                .get(i)
                .is_some_and(|next| next.is_alphanumeric() || matches!(next, '_' | '$'))
            {
                normalized.extend(&chars[start..i]);
            } else {
                normalized.push('?');
            }
            continue;
        }
        normalized.push(c);
        i += 1;
    }
    normalized.trim().to_owned()
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

fn connect_database(argument: &str) -> Option<String> {
    let (_, suffix) = argument.rsplit_once(" on ")?;
    let database = suffix
        .split_once(" using ")
        .map_or(suffix, |(name, _)| name)
        .trim();
    (!database.is_empty()).then(|| database.to_owned())
}

fn use_database(sql: &str) -> Option<String> {
    let rest = sql
        .trim()
        .strip_prefix("USE ")
        .or_else(|| sql.trim().strip_prefix("use "))?
        .trim();
    let name = rest.trim_end_matches(';').trim();
    if name.starts_with('`') && name.ends_with('`') && name.len() >= 2 {
        Some(name[1..name.len() - 1].replace("``", "`"))
    } else if !name.is_empty() && !name.chars().any(char::is_whitespace) {
        Some(name.to_owned())
    } else {
        None
    }
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
                thread_databases: HashMap::new(),
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
        let mut initialized_here = HashSet::new();
        let mut unknown_threads = HashSet::new();
        for row in &parsed {
            let id = row.identity.thread_id;
            match row.identity.command_type.as_str() {
                "Connect" | "Change user" | "Init DB" => {
                    initialized_here.insert(id);
                }
                "Quit" => {
                    initialized_here.remove(&id);
                }
                "Query" | "Execute"
                    if !state.thread_databases.contains_key(&id)
                        && !initialized_here.contains(&id)
                        && !state.own_threads.contains(&id) =>
                {
                    unknown_threads.insert(id);
                }
                _ => {}
            }
        }
        if !unknown_threads.is_empty() {
            // Older connections have no Connect record in the incremental batch.
            // Read their database changes once from the existing log before ingesting.
            let placeholders = vec!["?"; unknown_threads.len()].join(",");
            let sql = format!(
                "SELECT thread_id, command_type, LEFT(CAST(argument AS CHAR), 8192) AS argument \
                 FROM mysql.general_log WHERE event_time < ? AND thread_id IN ({placeholders}) \
                 AND (command_type IN ('Connect', 'Change user', 'Init DB', 'Quit') \
                      OR (command_type IN ('Query', 'Execute') AND UPPER(LEFT(CAST(argument AS CHAR), 4)) = 'USE ')) \
                 ORDER BY event_time, thread_id"
            );
            let mut query = sqlx::query(AssertSqlSafe(sql)).bind(&state.watermark);
            for id in &unknown_threads {
                query = query.bind(id);
            }
            let history = query
                .fetch_all(&mut *connection)
                .await
                .map_err(query_error)?;
            for row in history {
                let id: u64 = row.try_get("thread_id").map_err(query_error)?;
                let command: String = row.try_get("command_type").map_err(query_error)?;
                let argument: String = row.try_get("argument").map_err(query_error)?;
                state.update_thread_database(id, &command, &argument);
            }
            for id in unknown_threads {
                state.thread_databases.entry(id).or_insert(None);
            }
        }
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

    pub async fn sample_sql(&self, key: &QueryKey) -> Option<(Option<String>, String)> {
        self.state.lock().await.sample_sql(key)
    }

    pub async fn sample_sqls(&self, keys: &[QueryKey]) -> Vec<Option<(Option<String>, String)>> {
        let state = self.state.lock().await;
        keys.iter().map(|key| state.sample_sql(key)).collect()
    }

    pub async fn timeline(
        &self,
        query_key: Option<&QueryKey>,
        namespace: Option<&str>,
        hours: u32,
    ) -> Timeline {
        let scoped = namespace.map(|database| QueryKey(format!("db:{database}")));
        self.state.lock().await.timeline.series(
            TimelineBasis::ReceivedAtServer,
            query_key.or(scoped.as_ref()),
            hours,
        )
    }

    pub async fn timeline_range(
        &self,
        query_key: Option<&QueryKey>,
        namespace: Option<&str>,
        range: MinuteRange,
    ) -> Timeline {
        let scoped = namespace.map(|database| QueryKey(format!("db:{database}")));
        self.state.lock().await.timeline.series_range(
            TimelineBasis::ReceivedAtServer,
            query_key.or(scoped.as_ref()),
            range,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn state() -> CollectorState {
        CollectorState {
            watermark: "2026-10-07 00:00:00.000001".into(),
            seen_at_watermark: HashMap::new(),
            own_threads: vec![],
            thread_databases: HashMap::new(),
            ignored_username: "jansori".into(),
            statements: HashMap::new(),
            hosts: HashMap::new(),
            recent: VecDeque::new(),
            timeline: TimelineStore::default(),
            max_queries: 10,
            max_recent: 10,
            dropped_new_queries: 0,
        }
    }

    #[test]
    fn literals_are_grouped_without_changing_identifiers_or_operators() {
        assert_eq!(
            normalize_statement(
                "SELECT `item2` FROM orders_2024 WHERE id=12 AND name='a\\'b' AND price=1.25e-3"
            ),
            "SELECT `item2` FROM orders_2024 WHERE id=? AND name=? AND price=?"
        );
        assert_eq!(
            normalize_statement("SELECT * FROM orders_2025 WHERE id=13"),
            "SELECT * FROM orders_2025 WHERE id=?"
        );
        assert_eq!(
            normalize_statement("SELECT 'x' /*+ INDEX(t i1) */"),
            "SELECT ? /*+ INDEX(t i1) */"
        );
    }

    #[test]
    fn values_share_one_query_within_database_but_databases_stay_separate() {
        let mut state = state();
        let mut connect = row(
            "2026-10-07 00:00:00.000002",
            "app@host on shop using TCP/IP",
        );
        connect.identity.command_type = "Connect".into();
        let one = row(
            "2026-10-07 00:00:00.000003",
            "SELECT * FROM items WHERE id=1",
        );
        let two = row(
            "2026-10-07 00:00:00.000004",
            "SELECT * FROM items WHERE id=2",
        );
        let mut change = row("2026-10-07 00:00:00.000005", "archive");
        change.identity.command_type = "Init DB".into();
        let three = row(
            "2026-10-07 00:00:00.000006",
            "SELECT * FROM items WHERE id=3",
        );
        assert_eq!(state.ingest(vec![connect, one, two, change, three]), 3);
        let all = QueryFilter {
            limit: 10,
            ..QueryFilter::default()
        };
        let rows = state.query_stats(&all, QuerySort::Calls).unwrap();
        assert_eq!(rows.len(), 2);
        let shop = rows
            .iter()
            .find(|row| row.namespace.as_deref() == Some("shop"))
            .unwrap();
        assert_eq!(shop.calls, 2);
        assert_eq!(shop.statement_text, "SELECT * FROM items WHERE id=?");
        assert_eq!(
            state.sample_sql(&shop.key).unwrap().1,
            "SELECT * FROM items WHERE id=1"
        );
        let archive = rows
            .iter()
            .find(|row| row.namespace.as_deref() == Some("archive"))
            .unwrap();
        assert_eq!(archive.calls, 1);
        assert_ne!(shop.key, archive.key);
        let filter = QueryFilter {
            namespace: Some("shop".into()),
            limit: 10,
            ..QueryFilter::default()
        };
        assert_eq!(
            state.query_stats(&filter, QuerySort::Calls).unwrap().len(),
            1
        );
        assert_eq!(
            state
                .timeline
                .query_calls(MinuteRange::recent(1), "db:shop"),
            2
        );
        let hosts = state.host_stats(&HostFilter {
            namespace: Some("shop".into()),
            limit: 10,
            ..HostFilter::default()
        });
        assert_eq!(hosts[0].statement_calls, Some(2));
    }

    #[test]
    fn same_timestamp_rows_are_counted_once_across_polls() {
        let mut state = CollectorState {
            watermark: "2026-10-07 00:00:00.000001".into(),
            seen_at_watermark: HashMap::new(),
            own_threads: vec![],
            thread_databases: HashMap::new(),
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
            thread_databases: HashMap::new(),
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
