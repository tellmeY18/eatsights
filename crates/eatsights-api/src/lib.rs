//! Eatsights HTTP API: Poem + poem-openapi.

pub mod admin;
pub mod exports;
pub mod problem;
pub mod reads;
pub mod state;
pub mod writes;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use eatsights_core::schema::{PROPERTIES_JSON, PROPERTY_SCHEMA_JSON, SCHEMA_JSON};
use eatsights_db::{CHANGES_CHANNEL, Db};
use poem::http::StatusCode;
use poem::web::{Data, Path};
use poem::{Endpoint, EndpointExt, IntoResponse, Request, Response, Route, handler};
use poem_openapi::OpenApiService;
use tokio::sync::watch;

pub use state::{ApiConfig, AppState, Cidr};

fn service(
    base_url: &str,
) -> OpenApiService<
    (
        reads::ReadApi,
        exports::ExportApi,
        writes::WriteApi,
        admin::AdminApi,
    ),
    (),
> {
    OpenApiService::new(
        (
            reads::ReadApi,
            exports::ExportApi,
            writes::WriteApi,
            admin::AdminApi,
        ),
        "Eatsights API",
        env!("CARGO_PKG_VERSION"),
    )
    .description(
        "Open food data (CC0-1.0). The JSON Schema files at /schema/v0.3/ are the contract; \
             observation values are validated against the registry's value_schema at runtime.",
    )
    .license(poem_openapi::LicenseObject::new("CC0-1.0 (data)"))
    .server(base_url)
}

/// The OpenAPI document (JSON), independent of any database.
pub fn openapi_json() -> String {
    service(eatsights_core::BASE_URL).spec()
}

fn problem_response(status: u16, slug: &str, title: &str) -> Response {
    let body = serde_json::json!({
        "type": format!("{}{slug}", eatsights_core::PROBLEM_BASE),
        "title": title,
        "status": status,
    });
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .content_type("application/problem+json")
        .body(body.to_string())
}

#[handler]
fn openapi_doc(spec: Data<&Arc<String>>) -> Response {
    Response::builder()
        .content_type("application/json")
        .header("Cache-Control", "public, max-age=3600")
        .body(spec.0.as_str().to_owned())
}

#[handler]
fn schema_file(Path(file): Path<String>) -> Response {
    let body = match file.as_str() {
        "schema.json" => SCHEMA_JSON,
        "property.schema.json" => PROPERTY_SCHEMA_JSON,
        "properties.json" => PROPERTIES_JSON,
        _ => return problem_response(404, "not-found", "Not found"),
    };
    Response::builder()
        .content_type("application/schema+json")
        .header(
            "Cache-Control",
            "public, max-age=3600, stale-if-error=86400",
        )
        .body(body)
}

#[handler]
fn healthz() -> &'static str {
    "ok"
}

#[handler]
async fn readyz(st: Data<&Arc<AppState>>) -> Response {
    match st.db.ping().await {
        Ok(()) => "ready".into_response(),
        Err(_) => problem_response(503, "not-ready", "Database unavailable"),
    }
}

#[handler]
fn metrics(st: Data<&Arc<AppState>>) -> Response {
    Response::builder()
        .content_type("text/plain; version=0.0.4")
        .body(st.metrics.render(st.current_seq()))
}

/// Build the whole HTTP application.
pub fn app(state: Arc<AppState>) -> impl Endpoint {
    let api = service(&state.cfg.base_url);
    let spec = Arc::new(api.spec());
    let timeout = Duration::from_secs(state.cfg.request_timeout_secs.max(1));
    let limit = state.cfg.body_limit_bytes;
    let st2 = state.clone();
    let mut routes = Route::new();
    if state.cfg.swagger_ui {
        routes = routes.nest("/docs", api.swagger_ui());
    }
    routes
        .at("/openapi.json", openapi_doc.data(spec))
        .at("/schema/v0.3/:file", schema_file)
        .at("/healthz", healthz)
        .at("/readyz", readyz)
        .at("/metrics", metrics)
        .nest("/", api)
        .data(state)
        .around(move |ep, req: Request| {
            let st = st2.clone();
            async move {
                let too_big = req
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<usize>().ok())
                    .is_some_and(|n| n > limit);
                if too_big {
                    return Ok(problem_response(
                        413,
                        "body-too-large",
                        "Request body too large",
                    ));
                }
                let mut req = req;
                if !matches!(req.method().as_str(), "GET" | "HEAD") {
                    match tokio::time::timeout(timeout, req.take_body().into_bytes_limit(limit))
                        .await
                    {
                        Ok(Ok(b)) => req.set_body(b),
                        Err(_) => {
                            return Ok(problem_response(408, "timeout", "Request body timed out"));
                        }
                        Ok(Err(_)) => {
                            return Ok(problem_response(
                                413,
                                "body-too-large",
                                "Request body too large",
                            ));
                        }
                    }
                }
                let resp = match tokio::time::timeout(timeout, ep.get_response(req)).await {
                    Ok(r) => r,
                    Err(_) => problem_response(503, "timeout", "Request timed out"),
                };
                let m = &st.metrics;
                let c = match resp.status().as_u16() {
                    200..=299 => &m.requests_2xx,
                    300..=399 => &m.requests_3xx,
                    400..=499 => &m.requests_4xx,
                    _ => &m.requests_5xx,
                };
                state::Metrics::inc(c);
                Ok(resp)
            }
        })
}

/// Keep `seq` at the latest `public.changes.seq`: LISTEN on `eatsights_changes`,
/// with a polling fallback. Publishes every new value on `tx`.
pub async fn track_changes(
    db: Db,
    seq: Arc<AtomicI64>,
    tx: watch::Sender<i64>,
    mut shutdown: watch::Receiver<bool>,
    poll: Duration,
) {
    let mut listener = None;
    loop {
        if *shutdown.borrow() {
            break;
        }
        if listener.is_none() {
            match sqlx::postgres::PgListener::connect_with(&db.write).await {
                Ok(mut l) => match l.listen(CHANGES_CHANNEL).await {
                    Ok(()) => listener = Some(l),
                    Err(e) => tracing::warn!(error = %e, "LISTEN failed; polling"),
                },
                Err(e) => tracing::warn!(error = %e, "listener connect failed; polling"),
            }
        }
        match eatsights_db::read::latest_seq(&db.read).await {
            Ok(s) => {
                if s != seq.swap(s, Ordering::AcqRel) {
                    let _ = tx.send(s);
                }
            }
            Err(e) => tracing::warn!(error = %e, "reading latest seq failed"),
        }
        match listener.as_mut() {
            Some(l) => {
                tokio::select! {
                    r = tokio::time::timeout(poll, l.recv()) => {
                        if let Ok(Err(e)) = r {
                            tracing::warn!(error = %e, "listener error; reconnecting");
                            listener = None;
                        }
                    }
                    Ok(()) = shutdown.changed() => {}
                }
            }
            None => {
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    Ok(()) = shutdown.changed() => {}
                }
            }
        }
    }
}
