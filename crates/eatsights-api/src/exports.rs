//! Bulk snapshots: `GET /v1/export` (manifest) and `GET /v1/export/{name}` (gzip NDJSON).
//! Files are written by the worker (`eatsights_worker::export`); only names listed in the
//! latest manifest are served, so request paths never reach the filesystem.

use std::path::PathBuf;
use std::sync::Arc;

use eatsights_worker::export::{Manifest, latest_manifest, snapshot_manifest, valid_snapshot_name};
use poem::web::Data;
use poem::{Body, IntoResponse, Request};
use poem_openapi::param::Path;
use poem_openapi::payload::Payload;
use poem_openapi::registry::{MetaSchema, MetaSchemaRef};
use poem_openapi::{ApiResponse, OpenApi};
use serde_json::json;

use crate::problem::ProblemResp;
use crate::reads::{ApiTags, RawJson};
use crate::state::AppState;

/// `/v1/export/{snapshot}/{name}`: a snapshot never changes once written.
const SNAPSHOT_FILE_CACHE: &str = "public, max-age=31536000, immutable";
/// `/v1/export/{name}` follows `latest`, so it may change at any time: keep it short so a
/// CDN never pairs a fresh manifest with an old file.
const LATEST_FILE_CACHE: &str =
    "public, max-age=60, stale-while-revalidate=300, stale-if-error=604800";
const LIST_CACHE: &str = "public, max-age=300, stale-while-revalidate=3600, stale-if-error=604800";

/// A gzip-compressed NDJSON file (one JSON document per line).
pub struct GzipFile(pub Body);

impl Payload for GzipFile {
    const CONTENT_TYPE: &'static str = "application/gzip";
    fn schema_ref() -> MetaSchemaRef {
        MetaSchemaRef::Inline(Box::new(MetaSchema::new_with_format("string", "binary")))
    }
}

impl IntoResponse for GzipFile {
    fn into_response(self) -> poem::Response {
        poem::Response::builder()
            .content_type(Self::CONTENT_TYPE)
            .body(self.0)
    }
}

#[derive(ApiResponse)]
pub enum ExportListResp {
    /// The latest snapshot's manifest.
    #[oai(status = 200)]
    Ok(
        RawJson,
        #[oai(header = "ETag")] String,
        #[oai(header = "Cache-Control")] String,
    ),
    #[oai(status = 304)]
    NotModified(
        #[oai(header = "ETag")] String,
        #[oai(header = "Cache-Control")] String,
    ),
}

#[derive(ApiResponse)]
pub enum ExportFileResp {
    /// The file, gzip NDJSON. Its `ETag` is the SHA-256 listed in the manifest.
    #[oai(status = 200)]
    Ok(
        GzipFile,
        #[oai(header = "ETag")] String,
        #[oai(header = "Cache-Control")] String,
        #[oai(header = "Content-Disposition")] String,
    ),
    #[oai(status = 304)]
    NotModified(
        #[oai(header = "ETag")] String,
        #[oai(header = "Cache-Control")] String,
    ),
}

fn not_modified(req: &Request, etag: &str) -> bool {
    req.headers()
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|inm| inm.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
}

fn safe_file_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

async fn manifest(dir: PathBuf) -> Result<Manifest, ProblemResp> {
    tokio::task::spawn_blocking(move || latest_manifest(&dir))
        .await
        .map_err(ProblemResp::internal)?
        .ok_or_else(|| ProblemResp::not_found("export snapshot"))
}

pub struct ExportApi;

#[OpenApi(prefix_path = "/v1")]
impl ExportApi {
    /// Manifest of the latest public snapshot.
    ///
    /// Snapshots are written daily (and on demand with `eatsights export`) from one
    /// consistent database snapshot. They contain public data only: places, sections,
    /// menu_items, dishes, observations (public form), facts, vendor_of attestations
    /// and the registry, as gzip NDJSON, CC0-1.0.
    ///
    /// **Bootstrap a local copy:**
    /// 1. `GET /v1/export`; remember `changes_seq`.
    /// 2. Download every `files[].url` and check its `sha256`; load the NDJSON lines.
    /// 3. Follow `GET /v1/changes?after=<changes_seq>` and keep paging with
    ///    `after=<next_after>` until `items` is empty; re-fetch each changed entity.
    ///    The snapshot already contains everything up to and including `changes_seq`.
    #[oai(
        path = "/export",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "export_manifest"
    )]
    async fn list(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
    ) -> Result<ExportListResp, ProblemResp> {
        let m = manifest(st.cfg.export_dir.clone()).await?;
        let etag = format!("\"{}\"", m.snapshot);
        if not_modified(req, &etag) {
            return Ok(ExportListResp::NotModified(etag, LIST_CACHE.into()));
        }
        let base = st.cfg.base_url.trim_end_matches('/');
        let files: Vec<_> = m
            .files
            .iter()
            .map(|f| {
                json!({
                    "name": f.name, "bytes": f.bytes, "sha256": f.sha256, "records": f.records,
                    "url": format!("{base}/v1/export/{}/{}", m.snapshot, f.name),
                })
            })
            .collect();
        let body = json!({
            "snapshot": m.snapshot,
            "generated_at": m.generated_at,
            "changes_seq": m.changes_seq,
            "license": m.license,
            "files": files,
            "next": format!("{base}/v1/changes?after={}", m.changes_seq),
        });
        let bytes = serde_json::to_vec(&body).map_err(ProblemResp::internal)?;
        Ok(ExportListResp::Ok(
            RawJson(bytes.into()),
            etag,
            LIST_CACHE.into(),
        ))
    }

    /// One file of the latest snapshot, e.g. `places.ndjson.gz` (or just `places`).
    /// Served as `application/gzip`; `ETag` is the file's SHA-256.
    #[oai(
        path = "/export/:name",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "export"
    )]
    async fn file(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        name: Path<String>,
    ) -> Result<ExportFileResp, ProblemResp> {
        let dir = st.cfg.export_dir.clone();
        let m = manifest(dir.clone()).await?;
        serve_file(req, dir, &m, &name.0, LATEST_FILE_CACHE).await
    }

    /// One file of a specific snapshot, as linked from the manifest (`files[].url`).
    /// Immutable: cache it forever. Returns 404 once the snapshot is pruned (the newest
    /// few are kept); start again from `GET /v1/export`.
    #[oai(
        path = "/export/:snapshot/:name",
        method = "get",
        tag = "ApiTags::Feed",
        operation_id = "export_snapshot_file"
    )]
    async fn snapshot_file(
        &self,
        st: Data<&Arc<AppState>>,
        req: &Request,
        snapshot: Path<String>,
        name: Path<String>,
    ) -> Result<ExportFileResp, ProblemResp> {
        let dir = st.cfg.export_dir.clone();
        let snap = snapshot.0;
        let m = tokio::task::spawn_blocking({
            let dir = dir.clone();
            move || snapshot_manifest(&dir, &snap)
        })
        .await
        .map_err(ProblemResp::internal)?
        .ok_or_else(|| ProblemResp::not_found("export snapshot"))?;
        serve_file(req, dir, &m, &name.0, SNAPSHOT_FILE_CACHE).await
    }
}

async fn serve_file(
    req: &Request,
    dir: PathBuf,
    m: &Manifest,
    wanted: &str,
    cache: &str,
) -> Result<ExportFileResp, ProblemResp> {
    let Some(f) = m
        .files
        .iter()
        .find(|f| f.name == wanted || f.name == format!("{wanted}.ndjson.gz"))
        .filter(|f| safe_file_name(&f.name) && valid_snapshot_name(&m.snapshot))
    else {
        return Err(ProblemResp::not_found("export file"));
    };
    let etag = format!("\"{}\"", f.sha256);
    if not_modified(req, &etag) {
        return Ok(ExportFileResp::NotModified(etag, cache.into()));
    }
    let path = dir.join(&m.snapshot).join(&f.name);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ProblemResp::not_found("export file"))?;
    Ok(ExportFileResp::Ok(
        GzipFile(Body::from_async_read(file)),
        etag,
        cache.into(),
        format!("attachment; filename=\"{}\"", f.name),
    ))
}
