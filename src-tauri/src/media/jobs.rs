//! 媒体任务的生命周期：校验参数 → 解析凭据 → 提交 → 轮询 → 下载/写盘 → 更新 MediaJob，
//! 以及取消与应用重启后的恢复。状态全部写库，前端通过 `list_media_jobs` 读取。
//!
//! 安全约束：
//! - API Key 只从 CredentialVault 读取，只存在于内存里的 `Secret`，不进日志/错误/元数据；
//! - 写进 `MediaJob.error` 的文本会再做一次脱敏兜底（`redact_text`）；
//! - 上传文件先落盘到受控目录，任务结束即删除；产物写进 `media/<kind>/<uuid>.<ext>`。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use sqlx::SqlitePool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::adapter::{AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, OutputItem, PollState, RemoteTask, Secret, Submission};
use super::embedding::{self, EmbeddingResult};
use super::files::{self, MediaDirs, AUDIO_EXTS, IMAGE_EXTS};
use super::http::MediaHttp;
use super::registry::{AuthSpec, Capability, CapabilityConfig, MediaRegistry, Status};
use super::search::{self, FailedProvider, SearchHit};
use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::security::vault::{self, CredentialVault};
use crate::store::media::{self as repo, NewJob};
use crate::store::{new_id, now_ts, providers as providers_repo};

pub const ADAPTER_NOT_READY: &str = "适配器尚未接入";
pub const INTERRUPTED: &str = "应用重启中断";
const LIST_LIMIT: i64 = 200;
const MAX_STRING: usize = 1024;
const SUMMARY_CHARS: usize = 80;
const MAX_TEXT_RESULT_CHARS: usize = 200_000;
const MAX_ERROR_CHARS: usize = 400;
const MAX_POLL_ERRORS: u32 = 5;
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(20);

/// 各类产物的下载/写盘大小上限。
fn output_limit(kind: MediaKind) -> u64 {
    match kind {
        MediaKind::Image => 50 * 1024 * 1024,
        MediaKind::Tts | MediaKind::Asr => 100 * 1024 * 1024,
        MediaKind::Video => 1024 * 1024 * 1024,
        MediaKind::Search => 8 * 1024 * 1024,
    }
}

// ---------------------------------------------------------------------------------------------
// 上下文
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Timing {
    /// 覆盖所有能力的轮询间隔/超时（测试用）。
    pub poll_interval: Option<Duration>,
    pub poll_timeout: Option<Duration>,
    /// `submit` 返回前最多等待多久，让快速任务（搜索、TTS）直接带着结果返回。
    pub inline_wait: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { poll_interval: None, poll_timeout: None, inline_wait: Duration::from_millis(2500) }
    }
}

struct RunState {
    candidate: Candidate,
    remote: Option<(RemoteTask, Secret)>,
    /// 本任务已创建（或正在写入）的产物路径，失败/取消时清理。
    created: Vec<PathBuf>,
}

pub struct RunningJob {
    token: CancellationToken,
    state: Mutex<RunState>,
}

impl RunningJob {
    fn new(candidate: Candidate) -> Arc<Self> {
        Arc::new(RunningJob { token: CancellationToken::new(), state: Mutex::new(RunState { candidate, remote: None, created: Vec::new() }) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// 进程内正在执行的任务（用于取消）。
#[derive(Default)]
pub struct JobRegistry {
    running: Mutex<HashMap<String, Arc<RunningJob>>>,
}

impl JobRegistry {
    fn insert(&self, id: &str, job: Arc<RunningJob>) {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).insert(id.to_string(), job);
    }

    fn take(&self, id: &str) -> Option<Arc<RunningJob>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).remove(id)
    }

    fn contains(&self, id: &str) -> bool {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).contains_key(id)
    }
}

#[derive(Clone)]
pub struct MediaContext {
    pub pool: SqlitePool,
    pub vault: Arc<dyn CredentialVault>,
    pub dirs: MediaDirs,
    pub http: Arc<MediaHttp>,
    pub registry: Arc<MediaRegistry>,
    pub jobs: Arc<JobRegistry>,
    pub timing: Timing,
    lazy_recover: bool,
}

static HTTP: OnceLock<Arc<MediaHttp>> = OnceLock::new();
static JOBS: OnceLock<Arc<JobRegistry>> = OnceLock::new();
static REGISTRY: OnceLock<Arc<MediaRegistry>> = OnceLock::new();
static RECOVERED: AtomicBool = AtomicBool::new(false);

impl MediaContext {
    /// 生产环境上下文：HTTP 客户端、任务表、注册表在进程内共享，`app_data_dir` 下的 `media/` 存放文件。
    pub fn production(pool: SqlitePool, vault: Arc<dyn CredentialVault>, app_data_dir: &Path) -> AppResult<Self> {
        let http = match HTTP.get() {
            Some(h) => h.clone(),
            None => {
                let h = Arc::new(MediaHttp::new().map_err(|e| AppError::msg(e.to_string()))?);
                HTTP.get_or_init(|| h).clone()
            }
        };
        Ok(MediaContext {
            pool,
            vault,
            dirs: MediaDirs::new(app_data_dir),
            http,
            registry: REGISTRY.get_or_init(|| Arc::new(MediaRegistry::builtin().clone())).clone(),
            jobs: JOBS.get_or_init(|| Arc::new(JobRegistry::default())).clone(),
            timing: Timing::default(),
            lazy_recover: true,
        })
    }

    #[cfg(test)]
    pub fn for_test(pool: SqlitePool, vault: Arc<dyn CredentialVault>, dir: &Path, http: MediaHttp, registry: MediaRegistry) -> Self {
        MediaContext {
            pool,
            vault,
            dirs: MediaDirs::new(dir),
            http: Arc::new(http),
            registry: Arc::new(registry),
            jobs: Arc::new(JobRegistry::default()),
            timing: Timing {
                poll_interval: Some(Duration::from_millis(15)),
                poll_timeout: Some(Duration::from_secs(10)),
                inline_wait: Duration::from_secs(20),
            },
            lazy_recover: false,
        }
    }

    /// 每个进程只在首次使用媒体功能时恢复一次遗留任务。
    async fn ensure_recovered(&self) {
        if self.lazy_recover && !RECOVERED.swap(true, Ordering::SeqCst) {
            if let Err(e) = recover_interrupted(self).await {
                tracing::warn!("恢复遗留媒体任务失败：{e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 数据库（媒体任务表的更新；store/media.rs 目前只提供 insert/get/list/cancel）
// ---------------------------------------------------------------------------------------------

mod db {
    use super::*;

    #[derive(sqlx::FromRow)]
    pub struct ActiveRow {
        pub id: String,
        pub kind: String,
        pub provider_id: String,
        pub input_metadata_json: String,
    }

    pub async fn finish_success(pool: &SqlitePool, id: &str, summary: Option<&str>, output: Option<&str>) -> AppResult<bool> {
        let r = sqlx::query(
            "UPDATE media_jobs SET status = 'succeeded', summary = COALESCE(?2, summary), output_path = ?3, error = NULL, \
             finished_at = ?4 WHERE id = ?1 AND status IN ('queued', 'running')",
        )
        .bind(id)
        .bind(summary)
        .bind(output)
        .bind(now_ts())
        .execute(pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn finish_failed(pool: &SqlitePool, id: &str, error: &str) -> AppResult<bool> {
        let r = sqlx::query(
            "UPDATE media_jobs SET status = 'failed', error = ?2, finished_at = ?3 WHERE id = ?1 AND status IN ('queued', 'running')",
        )
        .bind(id)
        .bind(error)
        .bind(now_ts())
        .execute(pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn set_remote(pool: &SqlitePool, id: &str, task: &RemoteTask) -> AppResult<()> {
        sqlx::query(
            "UPDATE media_jobs SET input_metadata_json = json_set(input_metadata_json, '$.remote', json(?2)) \
             WHERE id = ?1 AND status IN ('queued', 'running')",
        )
        .bind(id)
        .bind(serde_json::to_string(task)?)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn set_progress(pool: &SqlitePool, id: &str, progress: u8) -> AppResult<()> {
        sqlx::query(
            "UPDATE media_jobs SET input_metadata_json = json_set(input_metadata_json, '$.progress', ?2) \
             WHERE id = ?1 AND status IN ('queued', 'running')",
        )
        .bind(id)
        .bind(progress as i64)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn list_active(pool: &SqlitePool) -> AppResult<Vec<ActiveRow>> {
        Ok(sqlx::query_as::<_, ActiveRow>(
            "SELECT id, kind, provider_id, input_metadata_json FROM media_jobs WHERE status IN ('queued', 'running') ORDER BY created_at",
        )
        .fetch_all(pool)
        .await?)
    }
}

// ---------------------------------------------------------------------------------------------
// 脱敏
// ---------------------------------------------------------------------------------------------

/// 兜底脱敏：去掉已知密钥、`Bearer xxx` 令牌，折叠换行并截断。
pub fn redact_text(msg: &str, secrets: &[&Secret]) -> String {
    let mut out = msg.to_string();
    for s in secrets {
        let key = s.expose();
        if key.len() >= 4 {
            out = out.replace(key, "[已隐藏]");
        }
    }
    // Bearer <token>
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(pos) = rest.to_ascii_lowercase().find("bearer ") {
        let (head, tail) = rest.split_at(pos + 7);
        result.push_str(head);
        let end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || "-._~+/=".contains(c))).unwrap_or(tail.len());
        if end >= 8 {
            result.push_str("[已隐藏]");
        } else {
            result.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    result.push_str(rest);
    let flat: String = result.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    super::adapter::util::truncate_chars(&flat, MAX_ERROR_CHARS)
}

// ---------------------------------------------------------------------------------------------
// 参数处理
// ---------------------------------------------------------------------------------------------

/// 递归把过长字符串替换为占位，避免 base64 之类写入数据库/日志。
fn sanitize(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > MAX_STRING => Value::String(format!("[omitted {} chars]", s.chars().count())),
        Value::Array(a) => Value::Array(a.iter().map(sanitize).collect()),
        Value::Object(o) if o.contains_key("dataBase64") => {
            // 文件载荷只留元数据
            json!({
                "name": o.get("name").and_then(Value::as_str).map(files::sanitize_file_name).unwrap_or_default(),
                "size": o.get("size").cloned().unwrap_or(Value::Null),
                "mimeType": o.get("mimeType").cloned().unwrap_or(Value::Null),
            })
        }
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), sanitize(v))).collect()),
        other => other.clone(),
    }
}

fn str_param<'a>(params: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| params.get(*k).and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty())
}

fn summarize(kind: MediaKind, params: &Map<String, Value>) -> String {
    match str_param(params, &["prompt", "text", "query", "input", "url"]) {
        Some(t) if t.chars().count() > SUMMARY_CHARS => format!("{}…", t.chars().take(SUMMARY_CHARS).collect::<String>()),
        Some(t) => t.to_string(),
        None => format!("{} 任务", kind.as_str()),
    }
}

/// 无论供应商能力表如何，这些字段是各能力的必需/核心字段，总是保留。
fn core_keys(cap: Capability) -> &'static [&'static str] {
    match cap {
        Capability::Image | Capability::Video => &["prompt", "mode"],
        Capability::Stt => &["timestamps", "diarization"],
        Capability::Tts => &["text", "voice"],
        Capability::Search => &["query", "resultMode"],
        Capability::Fetch => &["url"],
        Capability::Embedding => &["input"],
    }
}

/// 只带这些字段，未知/不被该上游支持的可选字段一律丢弃（并记录），避免向不支持的上游发送字段。
fn filter_params(cap: Capability, params: &Map<String, Value>, allowed: &[&CapabilityConfig]) -> (Map<String, Value>, Vec<String>) {
    // 这些是任务本身的簿记字段，丢弃但不算「被忽略的参数」
    const BOOKKEEPING: &[&str] = &["providerId", "provider_id", "provider", "model", "modelId", "engine", "intent", "image", "audio"];
    let mut kept = Map::new();
    let mut ignored = Vec::new();
    for (k, v) in params {
        if BOOKKEEPING.contains(&k.as_str()) {
            continue;
        }
        let empty = v.is_null() || v.as_str().map(|s| s.trim().is_empty()).unwrap_or(false);
        if core_keys(cap).contains(&k.as_str()) || allowed.iter().any(|c| c.has_param(k)) {
            if !empty {
                kept.insert(k.clone(), v.clone());
            }
        } else if !empty {
            ignored.push(k.clone());
        }
    }
    ignored.sort();
    (kept, ignored)
}

// ---------------------------------------------------------------------------------------------
// 凭据与适配器解析
// ---------------------------------------------------------------------------------------------

/// 一次调用的完整目标：供应商 + 能力配置 + 适配器 + 凭据（按账户优先级排序）。
#[derive(Clone)]
pub struct Candidate {
    pub provider_id: String,
    pub capability: Capability,
    pub cfg: CapabilityConfig,
    pub adapter: &'static dyn MediaAdapter,
    pub secrets: Vec<Secret>,
}

/// 读取该供应商所有可用账户的 API Key（只来自 CredentialVault）。
async fn resolve_secrets(ctx: &MediaContext, provider_id: &str) -> Result<Vec<Secret>, MediaError> {
    let conns = providers_repo::list_connections(&ctx.pool, provider_id)
        .await
        .map_err(|_| MediaError::Io("读取账户列表失败".into()))?;
    let mut out = Vec::new();
    let mut vault_err = None;
    for c in conns.into_iter().filter(|c| c.enabled && c.status == "active" && c.kind == "apiKey") {
        let Some(r) = c.credential_ref else { continue };
        match vault::get(&ctx.vault, &r).await {
            Ok(Some(s)) if !s.trim().is_empty() => out.push(Secret::new(s.trim())),
            Ok(_) => {}
            Err(e) => vault_err = Some(e.to_string()),
        }
    }
    if out.is_empty() {
        return Err(match vault_err {
            Some(e) => MediaError::Failed(format!("读取系统凭据库失败：{e}")),
            None => MediaError::NoCredential,
        });
    }
    Ok(out)
}

fn short_id(id: &str) -> String {
    super::adapter::util::truncate_chars(&id.chars().filter(|c| !c.is_control()).collect::<String>(), 40)
}

/// 检查供应商是否可用于该能力，并取出适配器；不读取凭据。
fn locate(ctx: &MediaContext, provider_id: &str, cap: Capability) -> Result<(CapabilityConfig, &'static dyn MediaAdapter), MediaError> {
    let not_ready = |why: &str| MediaError::Unsupported(format!("{ADAPTER_NOT_READY}：{why}"));
    let entry = ctx.registry.get(provider_id).ok_or_else(|| not_ready(&format!("未知供应商 {}", short_id(provider_id))))?;
    if entry.status == Some(Status::Deferred) {
        return Err(not_ready(entry.reason.as_deref().unwrap_or("该供应商暂不支持")));
    }
    let cfg = entry.capabilities.get(&cap).ok_or_else(|| not_ready(&format!("该供应商不提供{}能力", capability_cn(cap))))?;
    if !cfg.is_ready() {
        return Err(not_ready(cfg.reason.as_deref().unwrap_or("该能力暂不支持")));
    }
    let adapter = super::find_adapter(cap, &cfg.format).ok_or_else(|| not_ready(&format!("格式 {} 尚未实现", cfg.format)))?;
    Ok((cfg.clone(), adapter))
}

fn capability_cn(cap: Capability) -> &'static str {
    match cap {
        Capability::Image => "图像生成",
        Capability::Stt => "语音转文字",
        Capability::Tts => "文字转语音",
        Capability::Video => "视频生成",
        Capability::Search => "搜索",
        Capability::Fetch => "网页抓取",
        Capability::Embedding => "文本嵌入",
    }
}

pub async fn resolve_candidate(ctx: &MediaContext, provider_id: &str, cap: Capability) -> Result<Candidate, MediaError> {
    let (cfg, adapter) = locate(ctx, provider_id, cap)?;
    let secrets = if cfg.auth == AuthSpec::None { vec![Secret::new("")] } else { resolve_secrets(ctx, provider_id).await? };
    Ok(Candidate { provider_id: provider_id.to_string(), capability: cap, cfg, adapter, secrets })
}

// ---------------------------------------------------------------------------------------------
// 准备
// ---------------------------------------------------------------------------------------------

struct Plan {
    job_id: String,
    kind: MediaKind,
    model: String,
    candidates: Vec<Candidate>,
    aggregated: bool,
    req: MediaRequest,
    metadata: Value,
}

async fn prepare(ctx: &MediaContext, job_id: &str, kind: MediaKind, params: &Map<String, Value>) -> Result<Plan, MediaError> {
    let mut cap = Capability::for_kind(kind);
    if kind == MediaKind::Search && str_param(params, &["query"]).is_none() && str_param(params, &["url"]).is_some() {
        cap = Capability::Fetch;
    }
    let provider_id = str_param(params, &["providerId", "provider_id", "provider"]).ok_or_else(|| MediaError::InvalidParams("未选择供应商".into()))?;

    let primary = resolve_candidate(ctx, provider_id, cap).await;
    let aggregated = cap == Capability::Search && str_param(params, &["resultMode"]) == Some("aggregated");
    let mut candidates = Vec::new();
    if aggregated {
        // 聚合：主选供应商在前，再加上所有已配置凭据的搜索供应商
        let mut ids: Vec<String> = vec![provider_id.to_string()];
        ids.extend(ctx.registry.ready_provider_ids(Capability::Search).into_iter().filter(|i| *i != provider_id).map(str::to_string));
        for id in ids {
            if let Ok(c) = resolve_candidate(ctx, &id, Capability::Search).await {
                candidates.push(c);
            }
        }
        if candidates.is_empty() {
            // 没有任何可用来源：返回主选供应商的具体原因
            return Err(primary.err().unwrap_or(MediaError::NoCredential));
        }
    } else {
        candidates.push(primary?);
    }
    let main = &candidates[0];

    let model = str_param(params, &["model", "modelId"]).map(str::to_string).or_else(|| main.cfg.default_model.clone()).unwrap_or_default();
    let allowed: Vec<&CapabilityConfig> = candidates.iter().map(|c| &c.cfg).collect();
    let (kept, ignored) = filter_params(cap, params, &allowed);

    // 文件载荷先落盘（上限 25 MB），之后适配器只使用本地路径
    let mut uploaded = BTreeMap::new();
    for (field, exts) in [("image", IMAGE_EXTS), ("audio", AUDIO_EXTS)] {
        let wanted = match field {
            "image" => matches!(cap, Capability::Image | Capability::Video),
            _ => cap == Capability::Stt,
        };
        if !wanted {
            continue;
        }
        if let Some(payload) = params.get(field).filter(|v| !v.is_null()) {
            let f = ctx.dirs.save_upload(job_id, field, payload, exts).await?;
            uploaded.insert(field.to_string(), f);
        }
    }

    let files_meta: Map<String, Value> =
        uploaded.iter().map(|(k, f)| (k.clone(), json!({"mime": f.mime, "size": f.size, "ext": f.ext}))).collect();
    let metadata = json!({
        "params": sanitize(&Value::Object(params.clone())),
        "files": files_meta,
        "ignoredParams": ignored,
        "format": main.cfg.format,
        "aggregated": aggregated,
    });
    let req = MediaRequest { job_id: job_id.to_string(), provider_id: provider_id.to_string(), model: model.clone(), params: kept, files: uploaded };
    Ok(Plan { job_id: job_id.to_string(), kind, model, candidates, aggregated, req, metadata })
}

// ---------------------------------------------------------------------------------------------
// 对外接口：list / submit / cancel
// ---------------------------------------------------------------------------------------------

pub async fn list(ctx: &MediaContext, kind: MediaKind) -> AppResult<Vec<MediaJob>> {
    ctx.ensure_recovered().await;
    repo::list(&ctx.pool, kind, LIST_LIMIT).await
}

async fn get_job(pool: &SqlitePool, id: &str) -> AppResult<MediaJob> {
    repo::get(pool, id).await?.ok_or_else(|| AppError::NotFound(format!("media job {id}")))
}

pub async fn submit(ctx: &MediaContext, kind: MediaKind, params: Map<String, Value>) -> AppResult<MediaJob> {
    ctx.ensure_recovered().await;
    let id = new_id();
    let now = now_ts();
    let summary = summarize(kind, &params);
    let provider_id = str_param(&params, &["providerId", "provider_id", "provider"]).unwrap_or("").to_string();

    let plan = match prepare(ctx, &id, kind, &params).await {
        Ok(p) => p,
        Err(e) => {
            ctx.dirs.remove_uploads(&id).await;
            let model = str_param(&params, &["model", "modelId"]).unwrap_or("").to_string();
            let metadata = serde_json::to_string(&json!({"params": sanitize(&Value::Object(params))}))?;
            insert_job(ctx, &id, kind, &provider_id, &model, &summary, &metadata, JobStatus::Failed, Some(&e.to_string()), &now, Some(&now)).await?;
            return get_job(&ctx.pool, &id).await;
        }
    };

    let metadata = serde_json::to_string(&plan.metadata)?;
    insert_job(ctx, &id, kind, &provider_id, &plan.model, &summary, &metadata, JobStatus::Running, None, &now, None).await?;

    let running = RunningJob::new(plan.candidates[0].clone());
    ctx.jobs.insert(&id, running.clone());
    let worker = {
        let (ctx, running) = (ctx.clone(), running.clone());
        tokio::spawn(async move { run_plan(ctx, plan, running).await })
    };
    // 监护：任务线程 panic 时不能让作业永远停在「处理中」
    let guard = {
        let (ctx, id) = (ctx.clone(), id.clone());
        tokio::spawn(async move {
            if worker.await.is_err() {
                let _ = db::finish_failed(&ctx.pool, &id, "内部错误：任务异常终止").await;
                ctx.jobs.take(&id);
                ctx.dirs.remove_uploads(&id).await;
            }
        })
    };
    let _ = tokio::time::timeout(ctx.timing.inline_wait, guard).await;
    get_job(&ctx.pool, &id).await
}

#[allow(clippy::too_many_arguments)]
async fn insert_job(
    ctx: &MediaContext,
    id: &str,
    kind: MediaKind,
    provider_id: &str,
    model: &str,
    summary: &str,
    metadata: &str,
    status: JobStatus,
    error: Option<&str>,
    created_at: &str,
    finished_at: Option<&str>,
) -> AppResult<()> {
    repo::insert(
        &ctx.pool,
        &NewJob {
            id,
            kind,
            provider_id,
            model_id: model,
            summary,
            input_metadata_json: metadata,
            status,
            error,
            created_at,
            finished_at,
        },
    )
    .await
}

/// 取消仍在排队/运行的作业：先把库里的状态改成 cancelled（之后任务线程的写入都会被状态条件挡住），
/// 再中止本地等待并尽力取消上游任务。已结束的作业原样返回（幂等）。
pub async fn cancel(ctx: &MediaContext, id: &str) -> AppResult<MediaJob> {
    get_job(&ctx.pool, id).await?;
    let changed = repo::cancel_if_active(&ctx.pool, id, &now_ts()).await?;
    if changed {
        if let Some(running) = ctx.jobs.take(id) {
            running.token.cancel();
            let (remote, candidate) = {
                let st = running.lock();
                (st.remote.clone(), st.candidate.clone())
            };
            if let Some((task, secret)) = remote {
                let cx = AdapterCtx { http: &ctx.http, cfg: &candidate.cfg, secret: &secret, provider_id: &candidate.provider_id };
                match tokio::time::timeout(Duration::from_secs(10), candidate.adapter.cancel(&cx, &task)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(provider = %candidate.provider_id, "上游任务取消失败：{}", redact_text(&e.to_string(), &[&secret])),
                    Err(_) => tracing::warn!(provider = %candidate.provider_id, "上游任务取消超时"),
                }
            }
            cleanup_created(&running).await;
        }
        ctx.dirs.remove_uploads(id).await;
    }
    get_job(&ctx.pool, id).await
}

// ---------------------------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------------------------

struct Finished {
    output_path: Option<String>,
    text: Option<String>,
}

async fn cleanup_created(running: &RunningJob) {
    let paths = std::mem::take(&mut running.lock().created);
    for p in paths {
        let _ = tokio::fs::remove_file(&p).await;
        let mut part = p.clone().into_os_string();
        part.push(".part");
        let _ = tokio::fs::remove_file(PathBuf::from(part)).await;
    }
}

async fn run_plan(ctx: MediaContext, plan: Plan, running: Arc<RunningJob>) {
    let secrets: Vec<Secret> = plan.candidates.iter().flat_map(|c| c.secrets.iter().cloned()).collect();
    let result = tokio::select! {
        r = execute(&ctx, &plan, &running) => r,
        _ = running.token.cancelled() => Err(MediaError::Cancelled),
    };
    finalize(&ctx, &plan.job_id, &running, result, &secrets).await;
}

async fn finalize(ctx: &MediaContext, job_id: &str, running: &RunningJob, result: Result<Finished, MediaError>, secrets: &[Secret]) {
    match result {
        Ok(f) => {
            let text = f.text.as_deref().map(|t| super::adapter::util::truncate_chars(t, MAX_TEXT_RESULT_CHARS));
            match db::finish_success(&ctx.pool, job_id, text.as_deref(), f.output_path.as_deref()).await {
                Ok(true) => {}
                // 期间被取消了：取消优先，丢弃产物
                Ok(false) => cleanup_created(running).await,
                Err(e) => {
                    tracing::error!("写入任务结果失败：{e}");
                    cleanup_created(running).await;
                }
            }
        }
        Err(MediaError::Cancelled) => cleanup_created(running).await,
        Err(e) => {
            cleanup_created(running).await;
            let refs: Vec<&Secret> = secrets.iter().collect();
            let msg = redact_text(&e.to_string(), &refs);
            if let Err(db_err) = db::finish_failed(&ctx.pool, job_id, &msg).await {
                tracing::error!("写入任务失败状态失败：{db_err}");
            }
        }
    }
    ctx.jobs.take(job_id);
    ctx.dirs.remove_uploads(job_id).await;
}

fn polling(ctx: &MediaContext, cfg: &CapabilityConfig, cap: Capability) -> (Duration, Duration) {
    let (di, dt) = cap.default_polling();
    (
        ctx.timing.poll_interval.unwrap_or(Duration::from_millis(cfg.poll_interval_ms.unwrap_or(di))),
        ctx.timing.poll_timeout.unwrap_or(Duration::from_secs(cfg.poll_timeout_secs.unwrap_or(dt))),
    )
}

/// 依次用各账户的 Key 提交；鉴权失败/限流时换下一个账户。返回成功的结果和所用的 Key。
async fn submit_with_failover(ctx: &MediaContext, cand: &Candidate, req: &MediaRequest) -> Result<(Submission, Secret), MediaError> {
    let mut last = None;
    for secret in &cand.secrets {
        let cx = AdapterCtx { http: &ctx.http, cfg: &cand.cfg, secret, provider_id: &cand.provider_id };
        match cand.adapter.submit(&cx, req).await {
            Ok(s) => return Ok((s, secret.clone())),
            Err(e) if e.is_failover() => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or(MediaError::NoCredential))
}

async fn execute(ctx: &MediaContext, plan: &Plan, running: &Arc<RunningJob>) -> Result<Finished, MediaError> {
    let cand = &plan.candidates[0];
    let (outcome, secret) = if plan.aggregated {
        (aggregate(ctx, plan).await?, cand.secrets[0].clone())
    } else {
        let (sub, secret) = submit_with_failover(ctx, cand, &plan.req).await?;
        match sub {
            Submission::Done(o) => (o, secret),
            Submission::Pending(task) => {
                if let Err(e) = db::set_remote(&ctx.pool, &plan.job_id, &task).await {
                    tracing::warn!("保存上游任务句柄失败（重启后将无法恢复该任务）：{e}");
                }
                running.lock().remote = Some((task.clone(), secret.clone()));
                poll_until_done(ctx, &plan.job_id, cand, &task, &secret).await?
            }
        }
    };
    save_outcome(ctx, plan.kind, cand, &secret, running, outcome).await
}

/// 轮询到成功/失败/超时。瞬时错误（网络抖动、5xx）容忍有限次数；鉴权失败时换下一个账户。
async fn poll_until_done(ctx: &MediaContext, job_id: &str, cand: &Candidate, task: &RemoteTask, first: &Secret) -> Result<(Outcome, Secret), MediaError> {
    let (interval, timeout) = polling(ctx, &cand.cfg, cand.capability);
    let deadline = Instant::now() + timeout;
    let mut secret = first.clone();
    let mut tried_alt = 0usize;
    let mut transient = 0u32;
    let mut last_progress: Option<u8> = None;
    loop {
        tokio::time::sleep(interval).await;
        let cx = AdapterCtx { http: &ctx.http, cfg: &cand.cfg, secret: &secret, provider_id: &cand.provider_id };
        match cand.adapter.poll(&cx, task).await {
            Ok(PollState::Done(o)) => return Ok((o, secret)),
            Ok(PollState::Pending { progress }) => {
                transient = 0;
                if let Some(p) = progress.filter(|p| Some(*p) != last_progress) {
                    last_progress = Some(p);
                    let _ = db::set_progress(&ctx.pool, job_id, p).await;
                }
            }
            Err(e) if e.is_failover() && tried_alt + 1 < cand.secrets.len() => {
                tried_alt += 1;
                secret = cand.secrets[tried_alt].clone();
            }
            Err(e) if e.is_transient() && transient < MAX_POLL_ERRORS => transient += 1,
            Err(e) => return Err(e),
        }
        if Instant::now() >= deadline {
            return Err(MediaError::Failed(format!("等待上游结果超时（{} 秒）", timeout.as_secs())));
        }
    }
}

/// 把产物写进受控目录。`Remote` 项经 SSRF 防护下载；超过大小上限立即中止。
async fn save_outcome(ctx: &MediaContext, kind: MediaKind, cand: &Candidate, secret: &Secret, running: &RunningJob, outcome: Outcome) -> Result<Finished, MediaError> {
    let limit = output_limit(kind);
    let mut first: Option<PathBuf> = None;
    for item in outcome.outputs {
        let path = match item {
            OutputItem::Bytes { data, ext } => {
                if data.len() as u64 > limit {
                    return Err(MediaError::TooLarge(super::http::human_bytes(limit)));
                }
                let path = ctx.dirs.new_output_path(kind, &ext).await?;
                running.lock().created.push(path.clone());
                tokio::fs::write(&path, &data).await?;
                path
            }
            OutputItem::Remote { url, ext, authed } => {
                let path = ctx.dirs.new_output_path(kind, &ext).await?;
                running.lock().created.push(path.clone());
                let creds = authed.then_some((&cand.cfg, secret));
                ctx.http.download_to_file(&url, creds, limit, &path).await?;
                path
            }
        };
        // 以真实文件头校正扩展名（上游给的类型/文件名不可信）
        let fixed = files::finalize_ext(&path).await;
        if fixed != path {
            let mut st = running.lock();
            if let Some(slot) = st.created.iter_mut().find(|p| **p == path) {
                *slot = fixed.clone();
            }
        }
        first.get_or_insert(fixed);
    }
    Ok(Finished { output_path: first.map(|p| p.to_string_lossy().to_string()), text: outcome.text })
}

// ---------------------------------------------------------------------------------------------
// 聚合搜索
// ---------------------------------------------------------------------------------------------

fn hits_of(text: &str) -> Vec<SearchHit> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("results").and_then(Value::as_array).cloned())
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    SearchHit::new(
                        i.get("title").and_then(Value::as_str).unwrap_or(""),
                        i.get("url").and_then(Value::as_str).unwrap_or(""),
                        i.get("snippet").and_then(Value::as_str).unwrap_or(""),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 并发查询所有已配置的搜索来源，合并去重。仅当至少两个来源返回结果时才标 `aggregated`。
async fn aggregate(ctx: &MediaContext, plan: &Plan) -> Result<Outcome, MediaError> {
    let count = plan.req.u64_param("count").map(|c| c as usize).unwrap_or(search::DEFAULT_COUNT).clamp(1, search::MAX_COUNT);
    let mut set = JoinSet::new();
    for (idx, cand) in plan.candidates.iter().cloned().enumerate() {
        let (ctx, req) = (ctx.clone(), plan.req.clone());
        set.spawn(async move {
            let r = tokio::time::timeout(AGGREGATE_TIMEOUT, submit_with_failover(&ctx, &cand, &req)).await;
            let r = match r {
                Ok(Ok((Submission::Done(o), _))) => Ok(o.text.unwrap_or_default()),
                Ok(Ok(_)) => Err(MediaError::Unsupported("搜索接口不应返回长任务".into())),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(MediaError::Timeout),
            };
            let refs: Vec<&Secret> = cand.secrets.iter().collect();
            (idx, cand.provider_id.clone(), r.map_err(|e| redact_text(&e.to_string(), &refs)))
        });
    }
    let mut ok: Vec<(usize, String, Vec<SearchHit>)> = Vec::new();
    let mut failed = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, id, Ok(text))) => ok.push((idx, id, hits_of(&text))),
            Ok((_, id, Err(msg))) => failed.push(FailedProvider { provider: id, error: msg }),
            Err(_) => failed.push(FailedProvider { provider: "?".into(), error: "内部错误".into() }),
        }
    }
    ok.sort_by_key(|(i, _, _)| *i);
    failed.sort_by(|a, b| a.provider.cmp(&b.provider));
    if ok.is_empty() {
        let detail = failed.iter().map(|f| format!("{}：{}", f.provider, f.error)).collect::<Vec<_>>().join("；");
        return Err(MediaError::Failed(format!("所有搜索来源均失败：{detail}")));
    }
    let providers: Vec<String> = ok.iter().map(|(_, id, _)| id.clone()).collect();
    let origin = if ok.len() >= 2 { "aggregated" } else { "native" };
    let merged = search::merge_hits(ok.into_iter().map(|(_, _, h)| h).collect(), count);
    Ok(Outcome::text(search::payload_json(origin, &providers.join("+"), &merged, failed)))
}

// ---------------------------------------------------------------------------------------------
// 嵌入（不是 MediaKind，供网关/内部直接调用）
// ---------------------------------------------------------------------------------------------

pub async fn embed(ctx: &MediaContext, provider_id: &str, model: Option<&str>, inputs: Vec<String>, dimensions: Option<u32>) -> Result<EmbeddingResult, MediaError> {
    let cand = resolve_candidate(ctx, provider_id, Capability::Embedding).await?;
    let mut params = Map::new();
    params.insert("input".into(), json!(inputs));
    if let Some(d) = dimensions {
        params.insert("dimensions".into(), json!(d));
    }
    let model = model.map(str::trim).filter(|m| !m.is_empty()).map(str::to_string).or_else(|| cand.cfg.default_model.clone()).unwrap_or_default();
    let req = MediaRequest { job_id: String::new(), provider_id: provider_id.to_string(), model, params, files: BTreeMap::new() };
    let refs: Vec<&Secret> = cand.secrets.iter().collect();
    match submit_with_failover(ctx, &cand, &req).await {
        Ok((Submission::Done(o), _)) => embedding::parse_result(o.text.as_deref().unwrap_or("")),
        Ok(_) => Err(MediaError::Unsupported("嵌入接口不应返回长任务".into())),
        // 对外返回前脱敏兜底
        Err(e) => Err(redact_error(e, &refs)),
    }
}

fn redact_error(e: MediaError, secrets: &[&Secret]) -> MediaError {
    match e {
        MediaError::Upstream { status, message } => MediaError::Upstream { status, message: redact_text(&message, secrets) },
        MediaError::Network(m) => MediaError::Network(redact_text(&m, secrets)),
        MediaError::Failed(m) => MediaError::Failed(redact_text(&m, secrets)),
        MediaError::BadResponse(m) => MediaError::BadResponse(redact_text(&m, secrets)),
        other => other,
    }
}

// ---------------------------------------------------------------------------------------------
// 重启恢复
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverReport {
    pub resumed: usize,
    pub interrupted: usize,
}

/// 把上次运行遗留的 queued/running 任务处理掉：有上游任务句柄且凭据仍在的重新轮询，
/// 其余标记为 failed（「应用重启中断」）。同时清理孤儿上传目录。
pub async fn recover_interrupted(ctx: &MediaContext) -> AppResult<RecoverReport> {
    let mut report = RecoverReport::default();
    let mut keep = Vec::new();
    for row in db::list_active(&ctx.pool).await? {
        if ctx.jobs.contains(&row.id) {
            keep.push(row.id);
            continue; // 本进程正在执行
        }
        match try_resume(ctx, &row).await {
            Ok(()) => {
                report.resumed += 1;
                keep.push(row.id);
            }
            Err(reason) => {
                let msg = if reason.is_empty() { INTERRUPTED.to_string() } else { format!("{INTERRUPTED}（{reason}）") };
                db::finish_failed(&ctx.pool, &row.id, &msg).await?;
                report.interrupted += 1;
            }
        }
    }
    ctx.dirs.sweep_uploads(&keep).await;
    Ok(report)
}

/// Err(原因) 表示不能恢复；原因为空串表示根本没有可恢复的上游句柄。
async fn try_resume(ctx: &MediaContext, row: &db::ActiveRow) -> Result<(), String> {
    let kind = MediaKind::parse(&row.kind).ok_or_else(String::new)?;
    let meta: Value = serde_json::from_str(&row.input_metadata_json).map_err(|_| String::new())?;
    let task: RemoteTask = meta.get("remote").cloned().and_then(|r| serde_json::from_value(r).ok()).ok_or_else(String::new)?;
    let cap = Capability::for_kind(kind);
    let cand = resolve_candidate(ctx, &row.provider_id, cap).await.map_err(|e| format!("无法恢复：{e}"))?;
    if cand.cfg.format != task.format {
        return Err("无法恢复：供应商接口格式已变化".into());
    }
    let running = RunningJob::new(cand.clone());
    running.lock().remote = Some((task.clone(), cand.secrets[0].clone()));
    ctx.jobs.insert(&row.id, running.clone());

    let (ctx2, job_id) = (ctx.clone(), row.id.clone());
    let secrets = cand.secrets.clone();
    tokio::spawn(async move {
        let result = tokio::select! {
            r = async {
                let (outcome, secret) = poll_until_done(&ctx2, &job_id, &cand, &task, &cand.secrets[0]).await?;
                save_outcome(&ctx2, kind, &cand, &secret, &running, outcome).await
            } => r,
            _ = running.token.cancelled() => Err(MediaError::Cancelled),
        };
        finalize(&ctx2, &job_id, &running, result, &secrets).await;
    });
    Ok(())
}

/// 供命令层：某任务当前是否由本进程执行。
pub fn is_running(ctx: &MediaContext, id: &str) -> bool {
    ctx.jobs.contains(id)
}
