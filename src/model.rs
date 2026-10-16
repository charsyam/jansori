//! Database-neutral values displayed by the dashboard.

/// Identifies the SQL dialect.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum DatabaseKind {
    MySql,
    PostgreSql,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct DatabaseInfo {
    pub kind: DatabaseKind,
    pub version: String,
    pub display_name: String,
}

/// An unavailable optional source is reported instead of silently showing zeroes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Capability {
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Capabilities {
    pub read_only_connection: Capability,
    pub query_statistics: Capability,
    pub host_statistics: Capability,
    pub recent_statements: Capability,
    pub explain: Capability,
}

/// A stable key within one database connection. The engine owns its format.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize)]
pub struct QueryKey(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum TimelineBasis {
    /// The server timestamp when a statement was received.
    ReceivedAtServer,
    /// The collector's poll time for a change in cumulative counters.
    ObservedByCollector,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct TimeBucket {
    pub start_epoch_seconds: u64,
    pub calls: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Timeline {
    pub basis: TimelineBasis,
    pub bucket_seconds: u64,
    pub buckets: Vec<TimeBucket>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct QueryFilter {
    pub namespace: Option<String>,
    pub host: Option<String>,
    pub text_contains: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum QuerySort {
    Calls,
    /// Unavailable for MySQL general query log data.
    TotalTime,
    /// Unavailable for MySQL general query log data.
    MeanTime,
    /// Unavailable for MySQL general query log data.
    RowsExamined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum QuerySource {
    MySqlPerformanceSchema,
    MySqlGeneralLog,
    PostgreSqlStatStatements,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct QueryStats {
    pub source: QuerySource,
    pub key: QueryKey,
    pub namespace: Option<String>,
    /// A normalized statement. MySQL general log literals are shown as `?`.
    pub statement_text: String,
    pub calls: u64,
    /// `None` when the collection source does not record execution duration.
    pub total_time_ms: Option<f64>,
    pub mean_time_ms: Option<f64>,
    pub max_time_ms: Option<f64>,
    pub rows_examined: Option<u64>,
    pub rows_sent: Option<u64>,
    /// MySQL-only metric. `None` means unavailable, not zero.
    pub no_index_used: Option<u64>,
    /// MySQL-only metric. `None` means unavailable, not zero.
    pub no_good_index_used: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct BottleneckStats {
    pub key: String,
    pub namespace: String,
    pub statement_text: String,
    pub calls: u64,
    pub total_time_ms: f64,
    pub mean_time_ms: f64,
    pub rows_examined: u64,
    pub rows_sent: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct HostFilter {
    pub namespace: Option<String>,
    pub text_contains: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct HostStats {
    pub host: String,
    /// Current connections are a snapshot, not a historical total.
    pub current_connections: Option<u64>,
    /// `None` if the engine has no comparable cumulative connection counter.
    pub total_connections: Option<u64>,
    /// `None` if per-host statement totals are unavailable.
    pub statement_calls: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RecentStatement {
    pub source: QuerySource,
    pub query_key: Option<QueryKey>,
    pub namespace: Option<String>,
    pub sql: String,
    pub host: Option<String>,
    pub duration_ms: Option<u64>,
}

/// All EXPLAIN requests are estimates. Running the query is a separate feature.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ExplainRequest {
    pub namespace: Option<String>,
    pub sql: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum AccessKind {
    FullScan,
    IndexScan,
    IndexOnlyScan,
    BitmapScan,
    Other,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PlanNode {
    pub operation: String,
    pub relation: Option<String>,
    pub access: AccessKind,
    pub index_name: Option<String>,
    pub estimated_rows: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub children: Vec<PlanNode>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ExplainPlan {
    pub database: DatabaseKind,
    /// True for a parameter-independent PostgreSQL plan.
    pub generic: bool,
    pub root: PlanNode,
    /// Engine-native EXPLAIN FORMAT=JSON output, preserved for full detail.
    pub raw_json: String,
    /// Engine-native EXPLAIN output presented as its original rows and columns.
    pub raw_explain: ExplainOutput,
    /// Columns confidently resolved from a SELECT's top-level WHERE clause.
    pub where_columns: Vec<WhereColumn>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct WhereColumn {
    pub table: String,
    pub column: String,
    pub index_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ExplainOutput {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}
