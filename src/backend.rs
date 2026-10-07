//! Object-safe asynchronous interface implemented by each database adapter.

use std::future::Future;
use std::pin::Pin;

use crate::model::{
    Capabilities, DatabaseInfo, ExplainPlan, ExplainRequest, HostFilter, HostStats, QueryFilter,
    QueryKey, QuerySort, QueryStats, RecentStatement, Timeline,
};
use crate::timeline::MinuteRange;

pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, BackendError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendErrorKind {
    Connection,
    Permission,
    Unsupported,
    InvalidRequest,
    Query,
}

#[derive(Debug)]
pub struct BackendError {
    pub kind: BackendErrorKind,
    pub message: String,
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BackendError {}

/// Common surface used by collection jobs and HTTP handlers.
///
/// Implementations must bound query cost and avoid returning secrets in errors.
/// Explain implementations must reject statements that can write data and must
/// never use EXPLAIN ANALYZE for this method.
pub trait DatabaseBackend: Send + Sync {
    fn info(&self) -> BackendFuture<'_, DatabaseInfo>;
    fn capabilities(&self) -> BackendFuture<'_, Capabilities>;

    fn explain<'a>(&'a self, request: &'a ExplainRequest) -> BackendFuture<'a, ExplainPlan>;

    /// Plans only a statement already present in the collected statistics.
    fn explain_query<'a>(&'a self, key: &'a QueryKey) -> BackendFuture<'a, ExplainPlan> {
        Box::pin(async move {
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
            self.explain(&ExplainRequest {
                namespace: row.namespace,
                sql: row.statement_text,
            })
            .await
        })
    }

    /// A backend returns `Unsupported` when the requested sort needs a metric
    /// absent from its active collection source.
    fn query_stats<'a>(
        &'a self,
        filter: &'a QueryFilter,
        sort: QuerySort,
    ) -> BackendFuture<'a, Vec<QueryStats>>;
    fn query_stats_in_range<'a>(
        &'a self,
        filter: &'a QueryFilter,
        sort: QuerySort,
        range: MinuteRange,
    ) -> BackendFuture<'a, Vec<QueryStats>>;
    fn host_stats<'a>(&'a self, filter: &'a HostFilter) -> BackendFuture<'a, Vec<HostStats>>;
    fn recent_statements(&self, limit: u32) -> BackendFuture<'_, Vec<RecentStatement>>;

    fn timeline<'a>(
        &'a self,
        query_key: Option<&'a QueryKey>,
        hours: u32,
    ) -> BackendFuture<'a, Timeline>;
    fn timeline_range<'a>(
        &'a self,
        query_key: Option<&'a QueryKey>,
        range: MinuteRange,
    ) -> BackendFuture<'a, Timeline>;
}
