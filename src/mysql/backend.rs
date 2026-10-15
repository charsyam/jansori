//! MySQL implementation of the common backend contract.

use sqlx::{AssertSqlSafe, MySqlPool, Row};

use crate::backend::{BackendError, BackendErrorKind, BackendFuture, DatabaseBackend};
use crate::model::{
    AccessKind, Capabilities, Capability, DatabaseInfo, DatabaseKind, ExplainOutput, ExplainPlan,
    ExplainRequest, HostFilter, HostStats, PlanNode, QueryFilter, QueryKey, QuerySort, QueryStats,
    RecentStatement, Timeline,
};

use super::general_log::GeneralLogCollector;
use crate::timeline::MinuteRange;

pub struct MySqlBackend {
    pool: MySqlPool,
    general_log: GeneralLogCollector,
}

impl MySqlBackend {
    pub async fn new(pool: MySqlPool) -> Result<Self, BackendError> {
        let general_log = GeneralLogCollector::new(pool.clone(), 2_000, 5_000, 200).await?;
        Ok(Self { pool, general_log })
    }

    pub async fn poll_general_log(&self) -> Result<usize, BackendError> {
        self.general_log.poll().await
    }
}

fn query_error(error: sqlx::Error) -> BackendError {
    BackendError {
        kind: BackendErrorKind::Query,
        message: format!("MySQL 조회 실패: {error}"),
    }
}

fn json_error(error: serde_json::Error) -> BackendError {
    BackendError {
        kind: BackendErrorKind::Query,
        message: format!("MySQL 실행 계획 JSON 해석 실패: {error}"),
    }
}

fn capability(available: bool, reason: &str) -> Capability {
    Capability {
        available,
        reason: (!available).then(|| reason.to_owned()),
    }
}

impl DatabaseBackend for MySqlBackend {
    fn explain_query<'a>(&'a self, key: &'a QueryKey) -> BackendFuture<'a, ExplainPlan> {
        Box::pin(async move {
            let (namespace, sql) =
                self.general_log
                    .sample_sql(key)
                    .await
                    .ok_or_else(|| BackendError {
                        kind: BackendErrorKind::InvalidRequest,
                        message: "수집된 쿼리를 찾을 수 없습니다".into(),
                    })?;
            self.explain(&ExplainRequest { namespace, sql }).await
        })
    }

    fn databases(&self) -> BackendFuture<'_, Vec<String>> {
        Box::pin(async move {
            sqlx::query_scalar("SHOW DATABASES")
                .fetch_all(&self.pool)
                .await
                .map_err(query_error)
        })
    }

    fn info(&self) -> BackendFuture<'_, DatabaseInfo> {
        Box::pin(async move {
            let (version,): (String,) = sqlx::query_as("SELECT VERSION()")
                .fetch_one(&self.pool)
                .await
                .map_err(query_error)?;
            Ok(DatabaseInfo {
                kind: DatabaseKind::MySql,
                version,
                display_name: "MySQL".into(),
            })
        })
    }

    fn capabilities(&self) -> BackendFuture<'_, Capabilities> {
        Box::pin(async move {
            let log = self.general_log.is_available().await?;
            let read_only: i64 = sqlx::query_scalar("SELECT @@session.transaction_read_only")
                .fetch_one(&self.pool)
                .await
                .map_err(query_error)?;
            let reason = "general_log=ON 및 log_output=TABLE이 필요합니다";
            Ok(Capabilities {
                read_only_connection: capability(
                    read_only == 1,
                    "읽기 전용 트랜잭션 설정을 확인할 수 없습니다",
                ),
                query_statistics: capability(log, reason),
                host_statistics: capability(log, reason),
                recent_statements: capability(log, reason),
                explain: capability(log, reason),
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
            let mut connection = self.pool.acquire().await.map_err(query_error)?;
            if let Some(namespace) = &request.namespace {
                let escaped = namespace.replace('`', "``");
                // MySQL identifiers cannot be bound; doubled backticks keep the
                // user-provided namespace inside one quoted identifier.
                sqlx::raw_sql(AssertSqlSafe(format!("USE `{escaped}`")))
                    .execute(&mut *connection)
                    .await
                    .map_err(query_error)?;
            }
            // A statement cannot be bound as a value. Only one SELECT is
            // accepted above; deployment must also use a read-only account.
            let result: Result<_, sqlx::Error> = async {
                let (raw_json,): (String,) =
                    sqlx::query_as(AssertSqlSafe(format!("EXPLAIN FORMAT=JSON {sql}")))
                        .fetch_one(&mut *connection)
                        .await?;
                let rows = sqlx::query(AssertSqlSafe(format!("EXPLAIN {sql}")))
                    .fetch_all(&mut *connection)
                    .await?;
                let rows = rows
                    .iter()
                    .map(traditional_explain_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((raw_json, rows))
            }
            .await;
            // A changed default database must never leak to another pool borrower.
            if request.namespace.is_some() {
                connection.close().await.map_err(query_error)?;
            }
            let (raw_json, rows) = result.map_err(query_error)?;
            let value: serde_json::Value = serde_json::from_str(&raw_json).map_err(json_error)?;
            Ok(ExplainPlan {
                database: DatabaseKind::MySql,
                generic: false,
                root: plan_node(&value),
                raw_json,
                raw_explain: ExplainOutput {
                    columns: [
                        "id",
                        "select_type",
                        "table",
                        "partitions",
                        "type",
                        "possible_keys",
                        "key",
                        "key_len",
                        "ref",
                        "rows",
                        "filtered",
                        "Extra",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                    rows,
                },
            })
        })
    }

    fn query_stats<'a>(
        &'a self,
        filter: &'a QueryFilter,
        sort: QuerySort,
    ) -> BackendFuture<'a, Vec<QueryStats>> {
        Box::pin(async move { self.general_log.query_stats(filter, sort).await })
    }

    fn query_stats_in_range<'a>(
        &'a self,
        filter: &'a QueryFilter,
        sort: QuerySort,
        range: MinuteRange,
    ) -> BackendFuture<'a, Vec<QueryStats>> {
        Box::pin(async move {
            self.general_log
                .query_stats_in_range(filter, sort, range)
                .await
        })
    }

    fn host_stats<'a>(&'a self, filter: &'a HostFilter) -> BackendFuture<'a, Vec<HostStats>> {
        Box::pin(async move { Ok(self.general_log.host_stats(filter).await) })
    }

    fn recent_statements(&self, limit: u32) -> BackendFuture<'_, Vec<RecentStatement>> {
        Box::pin(async move { Ok(self.general_log.recent_statements(limit).await) })
    }

    fn timeline<'a>(
        &'a self,
        query_key: Option<&'a QueryKey>,
        namespace: Option<&'a str>,
        hours: u32,
    ) -> BackendFuture<'a, Timeline> {
        Box::pin(async move { Ok(self.general_log.timeline(query_key, namespace, hours).await) })
    }

    fn timeline_range<'a>(
        &'a self,
        query_key: Option<&'a QueryKey>,
        namespace: Option<&'a str>,
        range: MinuteRange,
    ) -> BackendFuture<'a, Timeline> {
        Box::pin(async move {
            Ok(self
                .general_log
                .timeline_range(query_key, namespace, range)
                .await)
        })
    }
}

fn traditional_explain_row(row: &sqlx::mysql::MySqlRow) -> Result<Vec<String>, sqlx::Error> {
    let text = |name| -> Result<String, sqlx::Error> {
        Ok(row
            .try_get::<Option<String>, _>(name)?
            .unwrap_or_else(|| "NULL".into()))
    };
    Ok(vec![
        row.try_get::<Option<u64>, _>("id")?
            .map_or_else(|| "NULL".into(), |value| value.to_string()),
        text("select_type")?,
        text("table")?,
        text("partitions")?,
        text("type")?,
        text("possible_keys")?,
        text("key")?,
        text("key_len")?,
        text("ref")?,
        row.try_get::<Option<u64>, _>("rows")?
            .map_or_else(|| "NULL".into(), |value| value.to_string()),
        row.try_get::<Option<f64>, _>("filtered")?
            .map_or_else(|| "NULL".into(), |value| format!("{value:.2}")),
        text("Extra")?,
    ])
}

fn plan_node(value: &serde_json::Value) -> PlanNode {
    let table = value.get("table").unwrap_or(value);
    let access_type = table
        .get("access_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let access = match access_type {
        "ALL" => AccessKind::FullScan,
        "index_merge" => AccessKind::BitmapScan,
        "index" | "range" | "ref" | "eq_ref" | "const" | "system" => AccessKind::IndexScan,
        _ => AccessKind::Other,
    };
    let mut children = Vec::new();
    if let Some(block) = value.get("query_block") {
        children.push(plan_node(block));
    }
    if let Some(nested) = value
        .get("nested_loop")
        .and_then(serde_json::Value::as_array)
    {
        children.extend(nested.iter().map(plan_node));
    }
    if let Some(grouping) = value.get("grouping_operation") {
        children.push(plan_node(grouping));
    }
    if let Some(ordering) = value.get("ordering_operation") {
        children.push(plan_node(ordering));
    }
    if let Some(duplicates) = value.get("duplicates_removal") {
        children.push(plan_node(duplicates));
    }
    PlanNode {
        operation: if !access_type.is_empty() {
            access_type.to_owned()
        } else {
            "query block".into()
        },
        relation: table
            .get("table_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        access,
        index_name: table
            .get("key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        estimated_rows: table
            .get("rows_examined_per_scan")
            .and_then(serde_json::Value::as_f64),
        estimated_cost: table
            .pointer("/cost_info/query_cost")
            .or_else(|| table.pointer("/cost_info/prefix_cost"))
            .and_then(serde_json::Value::as_str)
            .and_then(|cost| cost.parse().ok()),
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_index_and_scan_nodes() {
        let value = serde_json::json!({
            "query_block": {
                "nested_loop": [
                    {"table": {"table_name": "users", "access_type": "ref", "key": "idx_email"}},
                    {"table": {"table_name": "orders", "access_type": "ALL"}}
                ]
            }
        });
        let root = plan_node(&value);
        assert_eq!(root.children[0].children[0].access, AccessKind::IndexScan);
        assert_eq!(root.children[0].children[1].access, AccessKind::FullScan);
    }
}
