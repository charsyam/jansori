//! PostgreSQL implementation of the common backend contract.

use std::collections::HashMap;

use sqlx::{AssertSqlSafe, PgPool, Row};
use tokio::sync::Mutex;

use crate::backend::{BackendError, BackendErrorKind, BackendFuture, DatabaseBackend};
use crate::model::{
    AccessKind, Capabilities, Capability, DatabaseInfo, DatabaseKind, ExplainOutput, ExplainPlan,
    ExplainRequest, HostFilter, HostStats, PlanNode, QueryFilter, QueryKey, QuerySort, QuerySource,
    QueryStats, RecentStatement, Timeline, TimelineBasis,
};
use crate::timeline::{
    current_epoch_five_seconds, current_epoch_minute, MinuteRange, QueryMetric, TimelineStore,
};

pub struct PostgresBackend {
    pool: PgPool,
    timeline: Mutex<PgTimelineState>,
}

#[derive(Default)]
struct PgTimelineState {
    previous: HashMap<String, QueryMetric>,
    initialized: bool,
    store: TimelineStore,
}

impl PostgresBackend {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            timeline: Mutex::new(PgTimelineState::default()),
        }
    }

    async fn database_timeline_key(
        &self,
        namespace: Option<&str>,
    ) -> Result<Option<QueryKey>, BackendError> {
        let Some(database) = namespace else {
            return Ok(None);
        };
        let id: Option<i64> =
            sqlx::query_scalar("SELECT oid::bigint FROM pg_database WHERE datname = $1")
                .bind(database)
                .fetch_optional(&self.pool)
                .await
                .map_err(query_error)?;
        Ok(Some(QueryKey(format!("db:{}", id.unwrap_or(-1)))))
    }

    async fn statement_schema(&self) -> Result<Option<String>, BackendError> {
        sqlx::query_scalar(
            "SELECT n.nspname FROM pg_extension e \
             JOIN pg_namespace n ON n.oid = e.extnamespace \
             WHERE e.extname = 'pg_stat_statements'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(query_error)
    }

    /// Samples cumulative counters. A new counter is assigned to this poll's minute.
    pub async fn poll_timeline(&self) -> Result<u64, BackendError> {
        let schema = self
            .statement_schema()
            .await?
            .ok_or_else(|| unsupported("pg_stat_statements 확장이 필요합니다"))?;
        let sql = format!(
            "SELECT dbid::bigint, userid::bigint, queryid, calls, total_exec_time \
             FROM {}.pg_stat_statements \
             WHERE userid <> (SELECT oid FROM pg_roles WHERE rolname = current_user)",
            quote_identifier(&schema)
        );
        let rows: Vec<(i64, i64, Option<i64>, i64, f64)> = sqlx::query_as(AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        let mut next = HashMap::new();
        for (dbid, userid, queryid, calls, total_time_ms) in rows {
            next.insert(
                format!("{dbid}:{userid}:{queryid:?}"),
                QueryMetric {
                    calls: calls.max(0) as u64,
                    total_time_ms,
                },
            );
        }
        let mut state = self.timeline.lock().await;
        let mut added = 0;
        if state.initialized {
            let minute = current_epoch_minute();
            let recent_bucket = current_epoch_five_seconds();
            for (key, metric) in &next {
                let prior = state.previous.get(key).copied().unwrap_or_default();
                let delta = metric.calls.saturating_sub(prior.calls);
                let time_delta = (metric.total_time_ms - prior.total_time_ms).max(0.0);
                state.store.record(minute, key, delta, time_delta);
                state
                    .store
                    .record_recent(recent_bucket, key, delta, time_delta);
                if let Some(dbid) = key.split(':').next() {
                    state
                        .store
                        .record_scoped(minute, &format!("db:{dbid}"), delta);
                    state.store.record_recent_scoped(
                        recent_bucket,
                        &format!("db:{dbid}"),
                        delta,
                        time_delta,
                    );
                }
                added += delta;
            }
        }
        state.previous = next;
        state.initialized = true;
        Ok(added)
    }
}

fn query_error(error: sqlx::Error) -> BackendError {
    BackendError {
        kind: BackendErrorKind::Query,
        message: format!("PostgreSQL 조회 실패: {error}"),
    }
}

fn unsupported(message: &str) -> BackendError {
    BackendError {
        kind: BackendErrorKind::Unsupported,
        message: message.into(),
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

impl DatabaseBackend for PostgresBackend {
    fn databases(&self) -> BackendFuture<'_, Vec<String>> {
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT datname FROM pg_database WHERE datallowconn AND has_database_privilege(oid, 'CONNECT') ORDER BY datname",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)
        })
    }

    fn explain_query<'a>(&'a self, key: &'a QueryKey) -> BackendFuture<'a, ExplainPlan> {
        Box::pin(async move {
            let current: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(&self.pool)
                .await
                .map_err(query_error)?;
            let filter = QueryFilter {
                limit: 10_000,
                ..QueryFilter::default()
            };
            let row = self
                .query_stats(&filter, QuerySort::Calls)
                .await?
                .into_iter()
                .find(|row| &row.key == key)
                .ok_or_else(|| BackendError {
                    kind: BackendErrorKind::InvalidRequest,
                    message: "수집된 쿼리를 찾을 수 없습니다".into(),
                })?;
            if row.namespace.as_deref() != Some(&current) {
                return Err(unsupported("다른 PostgreSQL 데이터베이스의 실행 계획은 해당 데이터베이스에 직접 연결해야 조회할 수 있습니다"));
            }
            self.explain(&ExplainRequest {
                namespace: None,
                sql: row.statement_text,
            })
            .await
        })
    }

    fn info(&self) -> BackendFuture<'_, DatabaseInfo> {
        Box::pin(async move {
            let (version,): (String,) = sqlx::query_as("SELECT version()")
                .fetch_one(&self.pool)
                .await
                .map_err(query_error)?;
            Ok(DatabaseInfo {
                kind: DatabaseKind::PostgreSql,
                version,
                display_name: "PostgreSQL".into(),
            })
        })
    }

    fn capabilities(&self) -> BackendFuture<'_, Capabilities> {
        Box::pin(async move {
            let extension = self.statement_schema().await?.is_some();
            let read_only: String = sqlx::query_scalar("SHOW transaction_read_only")
                .fetch_one(&self.pool)
                .await
                .map_err(query_error)?;
            Ok(Capabilities {
                read_only_connection: Capability {
                    available: read_only == "on",
                    reason: (read_only != "on")
                        .then(|| "읽기 전용 트랜잭션 설정을 확인할 수 없습니다".into()),
                },
                query_statistics: Capability {
                    available: extension,
                    reason: (!extension).then(|| "pg_stat_statements 확장이 필요합니다".into()),
                },
                host_statistics: Capability {
                    available: true,
                    reason: None,
                },
                recent_statements: Capability {
                    available: false,
                    reason: Some(
                        "기본 PostgreSQL 통계에는 완료된 개별 문장 이력이 없습니다".into(),
                    ),
                },
                explain: Capability {
                    available: extension,
                    reason: (!extension).then(|| {
                        "수집된 쿼리의 실행 계획 조회에는 pg_stat_statements가 필요합니다".into()
                    }),
                },
            })
        })
    }

    fn explain<'a>(&'a self, request: &'a ExplainRequest) -> BackendFuture<'a, ExplainPlan> {
        Box::pin(async move {
            let sql = request.sql.trim().trim_end_matches(';').trim();
            if !sql.to_ascii_lowercase().starts_with("select ") || sql.contains(';') {
                return Err(BackendError {
                    kind: BackendErrorKind::InvalidRequest,
                    message: "단일 SELECT 문만 EXPLAIN할 수 있습니다".into(),
                });
            }
            let mut transaction = self.pool.begin().await.map_err(query_error)?;
            if let Some(namespace) = &request.namespace {
                let schema = quote_identifier(namespace);
                sqlx::query(AssertSqlSafe(format!("SET LOCAL search_path TO {schema}")))
                    .execute(&mut *transaction)
                    .await
                    .map_err(query_error)?;
            }
            let generic = has_parameters(sql);
            if generic {
                let version: i32 =
                    sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
                        .fetch_one(&mut *transaction)
                        .await
                        .map_err(query_error)?;
                if version < 160000 {
                    return Err(unsupported(
                        "이 PostgreSQL 버전은 자리표시자 쿼리의 범용 EXPLAIN을 지원하지 않습니다",
                    ));
                }
            }
            let options = if generic {
                "FORMAT JSON, GENERIC_PLAN TRUE"
            } else {
                "FORMAT JSON"
            };
            // A simple-protocol query lets PostgreSQL treat $1 as a generic
            // plan placeholder instead of a bind parameter from SQLx.
            let row = sqlx::raw_sql(AssertSqlSafe(format!("EXPLAIN ({options}) {sql}")))
                .fetch_one(&mut *transaction)
                .await
                .map_err(query_error)?;
            let value: serde_json::Value = row.try_get(0).map_err(query_error)?;
            let text_options = if generic {
                "FORMAT TEXT, GENERIC_PLAN TRUE"
            } else {
                "FORMAT TEXT"
            };
            let text_rows = sqlx::raw_sql(AssertSqlSafe(format!("EXPLAIN ({text_options}) {sql}")))
                .fetch_all(&mut *transaction)
                .await
                .map_err(query_error)?;
            let text_rows = text_rows
                .into_iter()
                .map(|row| {
                    row.try_get::<String, _>(0)
                        .map(|line| vec![line])
                        .map_err(query_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            transaction.rollback().await.map_err(query_error)?;
            let raw_json = value.to_string();
            let plan = value
                .get(0)
                .and_then(|item| item.get("Plan"))
                .ok_or_else(|| BackendError {
                    kind: BackendErrorKind::Query,
                    message: "PostgreSQL 실행 계획에 Plan 필드가 없습니다".into(),
                })?;
            Ok(ExplainPlan {
                database: DatabaseKind::PostgreSql,
                generic,
                root: plan_node(plan),
                raw_json,
                raw_explain: ExplainOutput {
                    columns: vec!["QUERY PLAN".into()],
                    rows: text_rows,
                },
            })
        })
    }

    fn query_stats<'a>(
        &'a self,
        filter: &'a QueryFilter,
        sort: QuerySort,
    ) -> BackendFuture<'a, Vec<QueryStats>> {
        Box::pin(async move {
            if filter.host.is_some() {
                return Err(unsupported(
                    "pg_stat_statements는 호스트별 쿼리 집계를 제공하지 않습니다",
                ));
            }
            if sort == QuerySort::RowsExamined {
                return Err(unsupported(
                    "pg_stat_statements에는 검사 행 수 지표가 없습니다",
                ));
            }
            let schema = self
                .statement_schema()
                .await?
                .ok_or_else(|| unsupported("pg_stat_statements 확장이 필요합니다"))?;
            let order = match sort {
                QuerySort::Calls => "s.calls",
                QuerySort::TotalTime => "s.total_exec_time",
                QuerySort::MeanTime => "s.mean_exec_time",
                QuerySort::RowsExamined => unreachable!(),
            };
            let sql = format!(
                "SELECT s.dbid::bigint, s.userid::bigint, s.queryid, d.datname, \
                 CASE WHEN s.query ~* '^[[:space:]]*(CREATE[[:space:]]+(USER|ROLE)|ALTER[[:space:]]+(USER|ROLE)|SET[[:space:]]|GRANT[[:space:]]|REVOKE[[:space:]])' \
                      THEN '[administrative statement redacted]' ELSE s.query END, s.calls, \
                 s.total_exec_time, s.mean_exec_time \
                 FROM {}.pg_stat_statements s JOIN pg_database d ON d.oid = s.dbid \
                 WHERE s.userid <> (SELECT oid FROM pg_roles WHERE rolname = current_user) \
                 AND ($1::text IS NULL OR s.query ILIKE '%' || $1 || '%') \
                 AND ($4::text IS NULL OR d.datname = $4) \
                 ORDER BY {order} DESC LIMIT $2 OFFSET $3",
                quote_identifier(&schema)
            );
            let rows: Vec<(i64, i64, Option<i64>, String, String, i64, f64, f64)> =
                sqlx::query_as(AssertSqlSafe(sql))
                    .bind(&filter.text_contains)
                    .bind(filter.limit as i64)
                    .bind(filter.offset as i64)
                    .bind(&filter.namespace)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(query_error)?;
            Ok(rows
                .into_iter()
                .map(
                    |(dbid, userid, queryid, database, query, calls, total, mean)| QueryStats {
                        source: QuerySource::PostgreSqlStatStatements,
                        key: QueryKey(format!("{dbid}:{userid}:{queryid:?}")),
                        namespace: Some(database),
                        statement_text: query,
                        calls: calls.max(0) as u64,
                        total_time_ms: Some(total),
                        mean_time_ms: Some(mean),
                        max_time_ms: None,
                        rows_examined: None,
                        rows_sent: None,
                        no_index_used: None,
                        no_good_index_used: None,
                    },
                )
                .collect())
        })
    }

    fn query_stats_in_range<'a>(
        &'a self,
        filter: &'a QueryFilter,
        sort: QuerySort,
        range: MinuteRange,
    ) -> BackendFuture<'a, Vec<QueryStats>> {
        Box::pin(async move {
            if sort == QuerySort::RowsExamined {
                return Err(unsupported(
                    "pg_stat_statements에는 검사 행 수 지표가 없습니다",
                ));
            }
            let mut all = filter.clone();
            all.limit = 10_000;
            all.offset = 0;
            let mut rows = self.query_stats(&all, QuerySort::Calls).await?;
            let state = self.timeline.lock().await;
            for row in &mut rows {
                let metric = state.store.query_metric(range, &row.key.0);
                row.calls = metric.calls;
                row.total_time_ms = Some(metric.total_time_ms);
                row.mean_time_ms =
                    (metric.calls > 0).then(|| metric.total_time_ms / metric.calls as f64);
            }
            rows.retain(|row| row.calls > 0);
            rows.sort_by(|a, b| {
                let order = match sort {
                    QuerySort::Calls => b.calls.cmp(&a.calls),
                    QuerySort::TotalTime => b
                        .total_time_ms
                        .partial_cmp(&a.total_time_ms)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    QuerySort::MeanTime => b
                        .mean_time_ms
                        .partial_cmp(&a.mean_time_ms)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    QuerySort::RowsExamined => unreachable!(),
                };
                order.then_with(|| a.key.0.cmp(&b.key.0))
            });
            Ok(rows
                .into_iter()
                .skip(filter.offset as usize)
                .take(filter.limit as usize)
                .collect())
        })
    }

    fn host_stats<'a>(&'a self, filter: &'a HostFilter) -> BackendFuture<'a, Vec<HostStats>> {
        Box::pin(async move {
            let rows: Vec<(String, i64)> = sqlx::query_as(
                "SELECT client_addr::text, COUNT(*) FROM pg_stat_activity \
                 WHERE client_addr IS NOT NULL \
                 AND ($1::text IS NULL OR client_addr::text LIKE '%' || $1 || '%') \
                 AND ($4::text IS NULL OR datname = $4) \
                 GROUP BY client_addr ORDER BY COUNT(*) DESC, client_addr::text \
                 LIMIT $2 OFFSET $3",
            )
            .bind(&filter.text_contains)
            .bind(filter.limit as i64)
            .bind(filter.offset as i64)
            .bind(&filter.namespace)
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
            Ok(rows
                .into_iter()
                .map(|(host, count)| HostStats {
                    host,
                    current_connections: Some(count.max(0) as u64),
                    total_connections: None,
                    statement_calls: None,
                })
                .collect())
        })
    }

    fn recent_statements(&self, _limit: u32) -> BackendFuture<'_, Vec<RecentStatement>> {
        Box::pin(async {
            Err(unsupported(
                "기본 PostgreSQL 통계에는 완료된 개별 문장 이력이 없습니다",
            ))
        })
    }

    fn timeline<'a>(
        &'a self,
        query_key: Option<&'a QueryKey>,
        namespace: Option<&'a str>,
        hours: u32,
    ) -> BackendFuture<'a, Timeline> {
        Box::pin(async move {
            let scoped = if query_key.is_none() {
                self.database_timeline_key(namespace).await?
            } else {
                None
            };
            Ok(self.timeline.lock().await.store.series(
                TimelineBasis::ObservedByCollector,
                query_key.or(scoped.as_ref()),
                hours,
            ))
        })
    }

    fn timeline_range<'a>(
        &'a self,
        query_key: Option<&'a QueryKey>,
        namespace: Option<&'a str>,
        range: MinuteRange,
    ) -> BackendFuture<'a, Timeline> {
        Box::pin(async move {
            let scoped = if query_key.is_none() {
                self.database_timeline_key(namespace).await?
            } else {
                None
            };
            Ok(self.timeline.lock().await.store.series_range(
                TimelineBasis::ObservedByCollector,
                query_key.or(scoped.as_ref()),
                range,
            ))
        })
    }
}

fn has_parameters(sql: &str) -> bool {
    sql.as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'$' && pair[1].is_ascii_digit())
}

fn plan_node(value: &serde_json::Value) -> PlanNode {
    let operation = value
        .get("Node Type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown")
        .to_owned();
    let access = match operation.as_str() {
        "Seq Scan" | "Parallel Seq Scan" => AccessKind::FullScan,
        "Index Scan" | "Index Scan Backward" => AccessKind::IndexScan,
        "Index Only Scan" => AccessKind::IndexOnlyScan,
        "Bitmap Heap Scan" | "Bitmap Index Scan" => AccessKind::BitmapScan,
        _ => AccessKind::Other,
    };
    PlanNode {
        operation,
        relation: value
            .get("Relation Name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        access,
        index_name: value
            .get("Index Name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        estimated_rows: value.get("Plan Rows").and_then(serde_json::Value::as_f64),
        estimated_cost: value.get("Total Cost").and_then(serde_json::Value::as_f64),
        children: value
            .get("Plans")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(plan_node)
            .collect(),
    }
}
