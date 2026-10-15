//! 一键接入：把网关地址 / 密钥 / 模型写进本机客户端（Codex、Claude Code）的配置文件。
//!
//! - 只改我们负责的字段，其余内容（注释、其它提供商、项目信任、其它设置）原样保留。
//! - 预览与写入分开：前端先 [`preview`] 给用户看会改什么，用户确认后才 [`apply`]。
//!   写入时重新计算，不信任预览时的结果。
//! - 写前自动备份；内容已是最新则不写、不备份。
//! - 现有文件无法解析时一律拒绝合并，绝不覆盖；错误信息不回显文件内容（可能含别家的密钥）。
//! - 配置文件是符号链接（常见于 dotfiles 管理）时写入真实目标，不替换链接本身。

pub mod claude;
pub mod codex;

#[cfg(test)]
mod tests;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Codex,
    Claude,
}

impl ClientKind {
    fn file_name(self) -> &'static str {
        match self {
            ClientKind::Codex => "config.toml",
            ClientKind::Claude => "settings.json",
        }
    }

    fn default_dir(self) -> &'static str {
        match self {
            ClientKind::Codex => ".codex",
            ClientKind::Claude => ".claude",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            ClientKind::Codex => "CODEX_HOME",
            ClientKind::Claude => "CLAUDE_CONFIG_DIR",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportInput {
    pub client: ClientKind,
    /// 网关根地址，形如 http://127.0.0.1:20128
    pub origin: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub haiku_model: Option<String>,
    #[serde(default)]
    pub sonnet_model: Option<String>,
    #[serde(default)]
    pub opus_model: Option<String>,
}

/// 校验并规整后的参数。
#[derive(Debug, Clone)]
pub(crate) struct Params {
    pub origin: String,
    pub key: String,
    pub model: String,
    pub haiku_model: String,
    pub sonnet_model: String,
    pub opus_model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub field: String,
    pub before: Option<String>,
    /// None = 删除该字段
    pub after: Option<String>,
}

pub(crate) struct Plan {
    pub content: String,
    pub changes: Vec<FieldChange>,
    /// 语义上已经是目标状态，无需写入
    pub unchanged: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub client: ClientKind,
    pub path: String,
    pub exists: bool,
    pub unchanged: bool,
    pub changes: Vec<FieldChange>,
    /// 有值表示不能导入（参数无效、文件无法解析等），界面应禁用确认按钮并显示原因。
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub path: String,
    pub backup_path: Option<String>,
    /// false = 内容本来就是最新，什么都没写
    pub changed: bool,
}

/// 展示旧值时打码：别家的密钥没必要完整出现在界面上。
pub(crate) fn mask(s: &str) -> String {
    let n = s.chars().count();
    if n <= 8 {
        "••••".to_string()
    } else {
        let head: String = s.chars().take(4).collect();
        let tail: String = s.chars().skip(n - 4).collect();
        format!("{head}…{tail}")
    }
}

fn no_control(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

fn validate(input: &ImportInput) -> Result<Params, String> {
    let url = Url::parse(input.origin.trim()).map_err(|_| "网关地址无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("网关地址需为 http / https 地址".into());
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err("网关地址只应包含协议、主机和端口".into());
    }
    let key = input.api_key.trim();
    if key.is_empty() || key.chars().count() > 512 || !no_control(key) {
        return Err("API 密钥为空或包含无效字符".into());
    }
    let model = input.model.trim();
    if model.is_empty() || model.chars().count() > 200 || !no_control(model) {
        return Err("模型名为空或包含无效字符".into());
    }
    let slot = |raw: Option<&String>| -> Result<String, String> {
        let name = raw.map(String::as_str).unwrap_or(model).trim();
        if name.is_empty() || name.chars().count() > 200 || !no_control(name) {
            return Err("模型名为空或包含无效字符".into());
        }
        Ok(name.to_string())
    };
    Ok(Params {
        origin: url.origin().ascii_serialization(),
        key: key.to_string(),
        model: model.to_string(),
        haiku_model: slot(input.haiku_model.as_ref())?,
        sonnet_model: slot(input.sonnet_model.as_ref())?,
        opus_model: slot(input.opus_model.as_ref())?,
    })
}

/// 配置文件路径：环境变量指定的绝对目录优先，否则 `<home>/.codex` / `<home>/.claude`。
pub fn resolve_path(kind: ClientKind, home: &Path, env_dir: Option<&str>) -> PathBuf {
    let dir = env_dir
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .unwrap_or_else(|| home.join(kind.default_dir()));
    dir.join(kind.file_name())
}

pub fn config_path(kind: ClientKind, home: &Path) -> PathBuf {
    resolve_path(kind, home, std::env::var(kind.env_var()).ok().as_deref())
}

fn read_existing(path: &Path) -> Result<Option<String>, String> {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => Err(format!("{} 是一个目录，不是配置文件", path.display())),
        Ok(m) if m.len() > MAX_CONFIG_BYTES => Err(format!("{} 过大，已拒绝自动修改", path.display())),
        Ok(_) => fs::read_to_string(path).map(Some).map_err(|e| format!("无法读取 {}：{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("无法访问 {}：{e}", path.display())),
    }
}

fn plan(kind: ClientKind, existing: Option<&str>, params: &Params) -> Result<Plan, String> {
    match kind {
        ClientKind::Codex => codex::plan(existing, params),
        ClientKind::Claude => claude::plan(existing, params),
    }
}

/// 只读：算出会改什么。任何失败都放进 `error`，不会返回 Err。
pub fn preview(path: &Path, input: &ImportInput) -> ImportPreview {
    let mut out = ImportPreview {
        client: input.client,
        path: path.display().to_string(),
        exists: false,
        unchanged: false,
        changes: Vec::new(),
        error: None,
    };
    let result = validate(input).and_then(|params| {
        let existing = read_existing(path)?;
        out.exists = existing.is_some();
        plan(input.client, existing.as_deref(), &params)
    });
    match result {
        Ok(p) => {
            out.unchanged = p.unchanged;
            out.changes = p.changes;
        }
        Err(e) => out.error = Some(e),
    }
    out
}

/// 写入（含备份）。内容已是最新时什么都不做。
pub fn apply(path: &Path, input: &ImportInput) -> Result<ImportOutcome, String> {
    let params = validate(input)?;
    let existing = read_existing(path)?;
    let plan = plan(input.client, existing.as_deref(), &params)?;
    let shown = path.display().to_string();
    if plan.unchanged {
        return Ok(ImportOutcome { path: shown, backup_path: None, changed: false });
    }

    // 符号链接：写真实文件，保住链接
    let target = if existing.is_some() {
        fs::canonicalize(path).map_err(|e| format!("无法解析 {} 的真实路径：{e}", path.display()))?
    } else {
        path.to_path_buf()
    };
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("无法创建目录 {}：{e}", dir.display()))?;
    }
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("config").to_string();

    let backup_path = if existing.is_some() {
        let backup = target.with_file_name(format!("{name}.router-bak-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        fs::copy(&target, &backup).map_err(|e| format!("备份原文件失败，已取消写入：{e}"))?;
        Some(backup)
    } else {
        None
    };

    let tmp = target.with_file_name(format!(".{name}.router-tmp"));
    let written = (|| -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // 文件里有密钥：新文件只给本人读写
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(plan.content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if existing.is_some() {
            // 沿用原文件的权限，不因为我们的写入而改变
            fs::set_permissions(&tmp, fs::metadata(&target)?.permissions())?;
        }
        fs::rename(&tmp, &target)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(format!("写入 {} 失败：{e}", target.display()));
    }
    Ok(ImportOutcome { path: shown, backup_path: backup_path.map(|b| b.display().to_string()), changed: true })
}
