//! 本地 HTTP 网关。归属：gateway agent。
//!
//! 与存储层的对接面（请勿随意改签名，存储层 agent 依赖它）：
//! - `KeyVerifier`：网关用它校验 Bearer 密钥，存储层负责实现（数据库只存哈希）。
//! - `GatewayManager`：启停与状态查询，Tauri 命令 start_gateway/stop_gateway 调用它。
//!
//! 内部结构：
//! - `server`：路由与中间件栈（访问日志 → 超时 → 认证/授权/限流 → 请求体大小检查）。
//! - `auth`：凭据校验、协议授权（403）、RPM 限流（429）。
//! - `rate_limit`：内存滑动窗口限流。
//! - `response`：协议风格错误体与请求分类。
//!
//! 转发管线：`/v1/chat/completions` 与 `/v1/messages` 在挂接了 [`crate::routing::Runtime`]
//! （`GatewayManager::set_runtime`）后走真实上游转发（见 `forward`）；`/v1/responses` 与未接入运行时的
//! 网关仍然在认证通过后返回 501，绝不伪造成功响应。
//! - `convert`：透传 / 规范化转换、`x-router-warnings`、上游错误 → 协议风格错误体。
//! - `stream`：首帧探测与拉取式（带背压）的流式转发。

mod auth;
pub(crate) mod collect;
pub(crate) mod convert;
mod forward;
mod rate_limit;
mod response;
mod server;
pub(crate) mod stream;

#[cfg(test)]
mod pipeline_tests;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::error::{AppError, AppResult};
use crate::routing::Runtime;

pub use auth::AuthenticatedKey;
pub use rate_limit::{RateLimited, RateLimiter};
pub use server::{GatewayConfig, DEFAULT_MAX_BODY_BYTES, DEFAULT_REQUEST_TIMEOUT, DEFAULT_SHUTDOWN_GRACE};

/// 校验通过后的密钥信息，供网关做协议/模型/限流判定。
#[derive(Debug, Clone)]
pub struct VerifiedKey {
    pub id: String,
    pub allowed_protocols: Vec<String>,
    pub allowed_models: Vec<String>,
    pub rpm_limit: Option<u32>,
    pub tpm_limit: Option<u32>,
}

pub type VerifyFuture = Pin<Box<dyn Future<Output = Option<VerifiedKey>> + Send>>;

pub trait KeyVerifier: Send + Sync + 'static {
    /// `bearer` 为请求中提供的完整明文密钥；已停用、已过期或不存在返回 None。
    fn verify(&self, bearer: String) -> VerifyFuture;
}

/// 与前端 `GatewayStatus`（src/types/index.ts）字段一致。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub address: String,
    pub port: u16,
    pub error: Option<String>,
}

struct Running {
    address: String,
    /// 实际监听端口（请求端口为 0 时是系统分配的端口）。
    port: u16,
    /// 发送（或丢弃）即触发 graceful shutdown。
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
    /// 服务任务异常退出时写入原因，供 `status()` 展示。
    exit_error: Arc<std::sync::Mutex<Option<String>>>,
    grace: Duration,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    last_error: Option<String>,
    last_address: String,
    last_port: u16,
}

impl Inner {
    /// 发现服务任务已经自行结束（异常退出）时，同步为「未运行」并记录原因。
    fn refresh(&mut self) {
        if self.running.as_ref().is_some_and(|r| r.join.is_finished()) {
            if let Some(r) = self.running.take() {
                let reason = r.exit_error.lock().ok().and_then(|mut e| e.take());
                self.last_error = Some(reason.unwrap_or_else(|| "网关服务意外退出".to_string()));
                self.last_address = r.address;
                self.last_port = r.port;
            }
        }
    }

    fn status(&self) -> GatewayStatus {
        match &self.running {
            Some(r) => GatewayStatus { running: true, address: r.address.clone(), port: r.port, error: None },
            None => GatewayStatus {
                running: false,
                address: self.last_address.clone(),
                port: self.last_port,
                error: self.last_error.clone(),
            },
        }
    }
}

#[derive(Default)]
pub struct GatewayManager {
    inner: Mutex<Inner>,
    /// 真实上游转发运行时；未设置时协议端点返回 501。启动网关时取当前值。
    runtime: std::sync::Mutex<Option<Arc<Runtime>>>,
}

fn describe_bind_error(e: &std::io::Error, address: &str, port: u16) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        AddrInUse => format!("端口 {port} 已被占用（{address}:{port}），请更换端口或关闭占用该端口的程序"),
        PermissionDenied => format!("没有权限监听 {address}:{port}（1024 以下端口通常需要管理员权限），请更换端口"),
        AddrNotAvailable => format!("无法在 {address} 上监听端口 {port}：该地址不属于本机的任何网卡"),
        _ => format!("无法启动网关（{address}:{port}）：{e}"),
    }
}

impl GatewayManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建时即挂接转发运行时。
    pub fn with_runtime(runtime: Arc<Runtime>) -> Self {
        let m = Self::default();
        m.set_runtime(runtime);
        m
    }

    /// 挂接（或替换）转发运行时。对之后启动的网关生效；已在运行的网关需要重启才会使用新运行时。
    pub fn set_runtime(&self, runtime: Arc<Runtime>) {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(runtime);
    }

    fn current_runtime(&self) -> Option<Arc<Runtime>> {
        self.runtime.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 转发运行时（供模型级探测这类只读命令复用真实适配器与凭据路径）。未挂接时为 None。
    pub fn runtime(&self) -> Option<Arc<Runtime>> {
        self.current_runtime()
    }

    /// 启动网关。端口被占用等绑定失败返回带端口号的 `AppError`（不 panic），并记入 `status().error`。
    /// 已在同一地址 / 端口运行时幂等返回当前状态；以不同地址 / 端口重复启动返回明确错误。
    /// `port == 0` 让系统分配端口，返回的状态里是实际端口。
    pub async fn start(&self, address: &str, port: u16, verifier: Arc<dyn KeyVerifier>) -> AppResult<GatewayStatus> {
        self.start_with_config(address, port, verifier, GatewayConfig::default()).await
    }

    /// 同 [`start`](Self::start)，可自定义请求体上限 / 超时等（主要用于测试）。
    pub async fn start_with_config(
        &self,
        address: &str,
        port: u16,
        verifier: Arc<dyn KeyVerifier>,
        config: GatewayConfig,
    ) -> AppResult<GatewayStatus> {
        let mut inner = self.inner.lock().await;
        inner.refresh();

        if let Some(r) = &inner.running {
            if r.address == address && (port == 0 || r.port == port) {
                return Ok(inner.status());
            }
            return Err(AppError::msg(format!(
                "本地网关已在 {}:{} 运行，请先停止后再使用新的地址或端口启动",
                r.address, r.port
            )));
        }

        let fail = |inner: &mut Inner, msg: String| -> AppError {
            inner.last_address = address.to_string();
            inner.last_port = port;
            inner.last_error = Some(msg.clone());
            AppError::msg(msg)
        };

        if address.trim().is_empty() {
            return Err(fail(&mut inner, format!("监听地址不能为空（端口 {port}）")));
        }
        let listener = match tokio::net::TcpListener::bind((address, port)).await {
            Ok(l) => l,
            Err(e) => {
                let msg = describe_bind_error(&e, address, port);
                tracing::warn!(address, port, kind = ?e.kind(), "gateway failed to bind");
                return Err(fail(&mut inner, msg));
            }
        };
        let actual_port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => return Err(fail(&mut inner, describe_bind_error(&e, address, port))),
        };
        if address.parse::<IpAddr>().is_ok_and(|ip| !ip.is_loopback()) {
            tracing::warn!(address, "gateway is listening on a non-loopback address; all API endpoints still require an API key");
        }

        let grace = config.shutdown_grace;
        let mut state = server::AppState::new(verifier, config);
        state.runtime = self.current_runtime();
        let app = server::build_router(state);
        let (tx, rx) = oneshot::channel::<()>();
        let exit_error = Arc::new(std::sync::Mutex::new(None));
        let exit_error_task = exit_error.clone();
        let join = tokio::spawn(async move {
            // 发送端被 drop 时 rx 同样返回，因此 GatewayManager 被销毁也会关闭服务。
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
            if let Err(e) = result {
                tracing::error!(error = %e, "gateway server stopped unexpectedly");
                if let Ok(mut slot) = exit_error_task.lock() {
                    *slot = Some(format!("网关服务异常退出：{e}"));
                }
            }
        });

        tracing::info!(address, port = actual_port, "gateway started");
        inner.last_error = None;
        inner.running = Some(Running {
            address: address.to_string(),
            port: actual_port,
            shutdown: Some(tx),
            join,
            exit_error,
            grace,
        });
        Ok(inner.status())
    }

    /// 停止网关：先 graceful shutdown 并等待在途请求，超过宽限期后强制终止。未运行时是空操作。
    pub async fn stop(&self) -> GatewayStatus {
        let mut inner = self.inner.lock().await;
        inner.refresh();
        if let Some(mut r) = inner.running.take() {
            if let Some(tx) = r.shutdown.take() {
                let _ = tx.send(());
            }
            if tokio::time::timeout(r.grace, &mut r.join).await.is_err() {
                tracing::warn!("gateway graceful shutdown timed out; aborting remaining connections");
                r.join.abort();
                let _ = (&mut r.join).await;
            }
            tracing::info!(address = %r.address, port = r.port, "gateway stopped");
            inner.last_address = r.address;
            inner.last_port = r.port;
        }
        inner.last_error = None;
        inner.status()
    }

    /// 反映真实运行状态：服务任务已结束则为未运行，并带上原因；绑定失败则带上次启动错误。
    pub async fn status(&self) -> GatewayStatus {
        let mut inner = self.inner.lock().await;
        inner.refresh();
        inner.status()
    }
}
