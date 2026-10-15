use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use jansori::backend::{BackendError, DatabaseBackend};
use jansori::model::{HostFilter, QueryFilter, QueryKey, QuerySort};
use jansori::mysql::backend::MySqlBackend;
use jansori::postgres::backend::PostgresBackend;
use jansori::timeline::{current_epoch_minute, MinuteRange, RETAIN_MINUTES};
use serde::Deserialize;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;

#[derive(Clone)]
struct AppState {
    backend: Arc<dyn DatabaseBackend>,
}

type ApiResult<T> = Result<Json<T>, ApiError>;

struct ApiError(BackendError);

impl From<BackendError> for ApiError {
    fn from(value: BackendError) -> Self {
        Self(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.kind {
            jansori::backend::BackendErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            jansori::backend::BackendErrorKind::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
            jansori::backend::BackendErrorKind::Permission => StatusCode::FORBIDDEN,
            _ => StatusCode::BAD_GATEWAY,
        };
        (status, Json(serde_json::json!({ "error": self.0.message }))).into_response()
    }
}

#[derive(Default, Deserialize)]
struct ListParams {
    namespace: Option<String>,
    host: Option<String>,
    search: Option<String>,
    sort: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    from: Option<u64>,
    to: Option<u64>,
    window: Option<String>,
}

fn selected_range(
    from: Option<u64>,
    to: Option<u64>,
    window: Option<&str>,
) -> Result<Option<MinuteRange>, ApiError> {
    let invalid = |message: &str| {
        ApiError(BackendError {
            kind: jansori::backend::BackendErrorKind::InvalidRequest,
            message: message.into(),
        })
    };
    if let Some(window) = window {
        if from.is_some() || to.is_some() {
            return Err(invalid(
                "조회 기간과 시작·끝 시각을 동시에 지정할 수 없습니다",
            ));
        }
        let minutes = match window {
            "1m" => 1,
            "5m" => 5,
            "10m" => 10,
            "1h" => 60,
            "3h" => 180,
            "1d" => 1_440,
            "3d" => 4_320,
            "7d" => RETAIN_MINUTES,
            _ => return Err(invalid("지원하지 않는 조회 기간입니다")),
        };
        return Ok(Some(MinuteRange::recent(minutes)));
    }
    match (from, to) {
        (None, None) => Ok(None),
        (Some(from), Some(to)) => {
            let range = MinuteRange::between(from / 60, to / 60);
            let now = current_epoch_minute();
            if range.start > range.end
                || range.end > now
                || range.start < now.saturating_sub(RETAIN_MINUTES - 1)
                || range.end - range.start >= RETAIN_MINUTES
            {
                return Err(ApiError(BackendError {
                    kind: jansori::backend::BackendErrorKind::InvalidRequest,
                    message: "시간 범위는 최근 7일 안에서 시작과 끝을 함께 지정해야 합니다".into(),
                }));
            }
            Ok(Some(range))
        }
        _ => Err(ApiError(BackendError {
            kind: jansori::backend::BackendErrorKind::InvalidRequest,
            message: "시작과 끝 시각을 함께 지정해야 합니다".into(),
        })),
    }
}

fn limit(value: Option<u32>) -> u32 {
    value.unwrap_or(50).clamp(1, 500)
}

const QUERY_PAGE_CAP: u32 = 10_000;

#[derive(serde::Serialize)]
struct QueryPage {
    items: Vec<jansori::model::QueryStats>,
    total: usize,
    total_is_exact: bool,
    limit: u32,
    offset: u32,
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn info(State(state): State<AppState>) -> ApiResult<jansori::model::DatabaseInfo> {
    Ok(Json(state.backend.info().await?))
}

async fn databases(State(state): State<AppState>) -> ApiResult<Vec<String>> {
    Ok(Json(state.backend.databases().await?))
}

async fn capabilities(State(state): State<AppState>) -> ApiResult<jansori::model::Capabilities> {
    Ok(Json(state.backend.capabilities().await?))
}

async fn queries(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> ApiResult<Vec<jansori::model::QueryStats>> {
    let rows = query_rows(
        &state,
        &params,
        limit(params.limit),
        params.offset.unwrap_or(0),
    )
    .await?;
    let rows = if selected_range(params.from, params.to, params.window.as_deref())?.is_none()
        && params.host.is_none()
    {
        state.backend.enrich_query_stats(rows).await?
    } else {
        rows
    };
    Ok(Json(rows))
}

async fn query_page(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> ApiResult<QueryPage> {
    let page_limit = limit(params.limit);
    let offset = params.offset.unwrap_or(0);
    let rows = query_rows(&state, &params, QUERY_PAGE_CAP + 1, 0).await?;
    let total_is_exact = rows.len() < QUERY_PAGE_CAP as usize;
    let total = rows.len().min(QUERY_PAGE_CAP as usize);
    let items = rows
        .into_iter()
        .skip(offset as usize)
        .take(page_limit as usize)
        .collect();
    let items = if selected_range(params.from, params.to, params.window.as_deref())?.is_none()
        && params.host.is_none()
    {
        state.backend.enrich_query_stats(items).await?
    } else {
        items
    };
    Ok(Json(QueryPage {
        items,
        total,
        total_is_exact,
        limit: page_limit,
        offset,
    }))
}

async fn query_rows(
    state: &AppState,
    params: &ListParams,
    row_limit: u32,
    offset: u32,
) -> Result<Vec<jansori::model::QueryStats>, ApiError> {
    let sort = match params.sort.as_deref().unwrap_or("calls") {
        "calls" => QuerySort::Calls,
        "total_time" => QuerySort::TotalTime,
        "mean_time" => QuerySort::MeanTime,
        "rows_examined" => QuerySort::RowsExamined,
        _ => {
            return Err(ApiError(BackendError {
                kind: jansori::backend::BackendErrorKind::InvalidRequest,
                message: "지원하지 않는 정렬 기준입니다".into(),
            }));
        }
    };
    let filter = QueryFilter {
        namespace: params.namespace.clone(),
        host: params.host.clone(),
        text_contains: params.search.clone(),
        limit: row_limit,
        offset,
    };
    match selected_range(params.from, params.to, params.window.as_deref())? {
        Some(range) => Ok(state
            .backend
            .query_stats_in_range(&filter, sort, range)
            .await?),
        None => Ok(state.backend.query_stats(&filter, sort).await?),
    }
}

async fn hosts(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> ApiResult<Vec<jansori::model::HostStats>> {
    let filter = HostFilter {
        namespace: params.namespace,
        text_contains: params.search,
        limit: limit(params.limit),
        offset: params.offset.unwrap_or(0),
    };
    Ok(Json(state.backend.host_stats(&filter).await?))
}

async fn recent(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> ApiResult<Vec<jansori::model::RecentStatement>> {
    Ok(Json(
        state.backend.recent_statements(limit(params.limit)).await?,
    ))
}

#[derive(Default, Deserialize)]
struct TimelineParams {
    query_key: Option<String>,
    namespace: Option<String>,
    hours: Option<u32>,
    from: Option<u64>,
    to: Option<u64>,
    window: Option<String>,
}

async fn timeline(
    State(state): State<AppState>,
    Query(params): Query<TimelineParams>,
) -> ApiResult<jansori::model::Timeline> {
    let key = params.query_key.map(QueryKey);
    match selected_range(params.from, params.to, params.window.as_deref())? {
        Some(range) => Ok(Json(
            state
                .backend
                .timeline_range(key.as_ref(), params.namespace.as_deref(), range)
                .await?,
        )),
        None => Ok(Json(
            state
                .backend
                .timeline(
                    key.as_ref(),
                    params.namespace.as_deref(),
                    params.hours.unwrap_or(24).clamp(1, 168),
                )
                .await?,
        )),
    }
}

async fn explain(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<jansori::model::ExplainPlan> {
    Ok(Json(state.backend.explain_query(&QueryKey(key)).await?))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let bind: SocketAddr = std::env::var("JANSORI_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    let poll_ms: u64 = std::env::var("JANSORI_POLL_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000);
    let backend: Arc<dyn DatabaseBackend>;

    if database_url.starts_with("mysql://") {
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .after_connect(|connection, _meta| {
                Box::pin(async move {
                    sqlx::query("SET SESSION TRANSACTION READ ONLY")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await?;
        let read_only: i64 = sqlx::query_scalar("SELECT @@session.transaction_read_only")
            .fetch_one(&pool)
            .await?;
        if read_only != 1 {
            return Err("MySQL 연결이 읽기 전용으로 설정되지 않았습니다".into());
        }
        let mysql = Arc::new(MySqlBackend::new(pool).await?);
        let collector = mysql.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(poll_ms.max(100)));
            let mut previous_error = String::new();
            loop {
                timer.tick().await;
                match collector.poll_general_log().await {
                    Ok(_) => previous_error.clear(),
                    Err(error) if error.message != previous_error => {
                        eprintln!("general log: {error}");
                        previous_error = error.message;
                    }
                    Err(_) => {}
                }
            }
        });
        backend = mysql;
    } else if database_url.starts_with("postgres://") || database_url.starts_with("postgresql://") {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(|connection, _meta| {
                Box::pin(async move {
                    sqlx::query("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await?;
        let read_only: String = sqlx::query_scalar("SHOW transaction_read_only")
            .fetch_one(&pool)
            .await?;
        if read_only != "on" {
            return Err("PostgreSQL 연결이 읽기 전용으로 설정되지 않았습니다".into());
        }
        let postgres = Arc::new(PostgresBackend::new(pool));
        let collector = postgres.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(poll_ms.max(5_000)));
            let mut previous_error = String::new();
            loop {
                timer.tick().await;
                match collector.poll_timeline().await {
                    Ok(_) => previous_error.clear(),
                    Err(error) if error.message != previous_error => {
                        eprintln!("PostgreSQL timeline: {error}");
                        previous_error = error.message;
                    }
                    Err(_) => {}
                }
            }
        });
        backend = postgres;
    } else {
        return Err("DATABASE_URL은 mysql:// 또는 postgres:// 형식이어야 합니다".into());
    }

    let state = AppState { backend };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/info", get(info))
        .route("/api/databases", get(databases))
        .route("/api/capabilities", get(capabilities))
        .route("/api/queries", get(queries))
        .route("/api/queries/page", get(query_page))
        .route("/api/hosts", get(hosts))
        .route("/api/recent", get(recent))
        .route("/api/timeline", get(timeline))
        .route("/api/queries/{key}/explain", get(explain))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Jansori: http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
