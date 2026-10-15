use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use toml_edit::DocumentMut;

use super::*;

const KEY: &str = "sk-r-NEWKEY-0123456789";
const ORIGIN: &str = "http://127.0.0.1:20128";

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("router-int-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn input(client: ClientKind) -> ImportInput {
    ImportInput { client, origin: ORIGIN.into(), api_key: KEY.into(), model: "openai/gpt-4o-mini".into(), haiku_model: None, sonnet_model: None, opus_model: None }
}

fn params() -> Params {
    validate(&input(ClientKind::Codex)).unwrap()
}

fn toml(s: &str) -> DocumentMut {
    s.parse().unwrap_or_else(|e| panic!("输出不是合法 TOML: {e}\n{s}"))
}

fn backups(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".router-bak-"))
        .collect();
    v.sort();
    v
}

const CODEX_SAMPLE: &str = r#"# 我的 Codex 配置
model_provider = "custom"
model = "old-model" # 行尾注释要保留
notify = ["notifier", "turn-ended"]

[model_providers.custom]
name = "OpenAI"
base_url = "https://example.com"
wire_api = "responses"
experimental_bearer_token = "sk-old-secret-token-ABCD"

[profiles.m21]
model = "codex-MiniMax-M2.5"

[projects."/Users/x/proj"]
trust_level = "trusted"
"#;

// ───────── Codex ─────────

#[test]
fn codex_merge_keeps_everything_else() {
    let plan = codex::plan(Some(CODEX_SAMPLE), &params()).unwrap();
    let doc = toml(&plan.content);
    assert_eq!(doc["model_provider"].as_str(), Some("freeapibox"));
    assert_eq!(doc["model"].as_str(), Some("openai/gpt-4o-mini"));
    let r = &doc["model_providers"]["freeapibox"];
    assert_eq!(r["base_url"].as_str(), Some("http://127.0.0.1:20128/v1"));
    assert_eq!(r["wire_api"].as_str(), Some("responses"));
    assert_eq!(r["experimental_bearer_token"].as_str(), Some(KEY));
    assert_eq!(r["name"].as_str(), Some("FreeApiBox"));

    // 原有内容原样保留
    assert!(plan.content.contains("# 我的 Codex 配置"));
    assert!(plan.content.contains(r#"model = "openai/gpt-4o-mini" # 行尾注释要保留"#), "{}", plan.content);
    assert!(plan.content.contains(r#"notify = ["notifier", "turn-ended"]"#));
    assert_eq!(doc["model_providers"]["custom"]["experimental_bearer_token"].as_str(), Some("sk-old-secret-token-ABCD"));
    assert_eq!(doc["profiles"]["m21"]["model"].as_str(), Some("codex-MiniMax-M2.5"));
    assert_eq!(doc["projects"]["/Users/x/proj"]["trust_level"].as_str(), Some("trusted"));
    assert!(plan.content.find("[model_providers.custom]").unwrap() < plan.content.find("[model_providers.freeapibox]").unwrap());

    let model = plan.changes.iter().find(|c| c.field == "model").unwrap();
    assert_eq!((model.before.as_deref(), model.after.as_deref()), (Some("old-model"), Some("openai/gpt-4o-mini")));
    let provider = plan.changes.iter().find(|c| c.field == "model_provider").unwrap();
    assert_eq!(provider.before.as_deref(), Some("custom"));
    assert!(!plan.unchanged);
}

#[test]
fn codex_fresh_file_has_only_what_we_need() {
    let plan = codex::plan(None, &params()).unwrap();
    let doc = toml(&plan.content);
    assert_eq!(doc["model_provider"].as_str(), Some("freeapibox"));
    assert_eq!(doc["model_providers"]["freeapibox"]["experimental_bearer_token"].as_str(), Some(KEY));
    assert!(plan.content.contains("[model_providers.freeapibox]"));
    assert!(!plan.content.contains("[model_providers]\n"), "不应输出空的 [model_providers] 表头:\n{}", plan.content);
    assert!(plan.changes.iter().all(|c| c.before.is_none()));
}

#[test]
fn codex_second_import_is_unchanged() {
    let first = codex::plan(Some(CODEX_SAMPLE), &params()).unwrap();
    let second = codex::plan(Some(&first.content), &params()).unwrap();
    assert!(second.unchanged);
    assert_eq!(second.content, first.content);
}

#[test]
fn codex_updates_existing_router_masks_old_token_and_drops_env_key() {
    let existing = r#"model_provider = "router"
model = "m"

[model_providers.freeapibox]
name = "x"
base_url = "http://old:1/v1"
wire_api = "responses"
env_key = "ROUTER_API_KEY"
experimental_bearer_token = "sk-previous-token-WXYZ"
"#;
    let plan = codex::plan(Some(existing), &params()).unwrap();
    let doc = toml(&plan.content);
    let r = &doc["model_providers"]["freeapibox"];
    assert_eq!(r["base_url"].as_str(), Some("http://127.0.0.1:20128/v1"));
    assert_eq!(r["experimental_bearer_token"].as_str(), Some(KEY));
    assert!(r.get("env_key").is_none());

    let token = plan.changes.iter().find(|c| c.field.ends_with("experimental_bearer_token")).unwrap();
    assert_eq!(token.before.as_deref(), Some("sk-p…WXYZ"));
    assert_eq!(token.after.as_deref(), Some(KEY));
    let env_key = plan.changes.iter().find(|c| c.field.ends_with("env_key")).unwrap();
    assert_eq!((env_key.before.as_deref(), env_key.after.as_deref()), (Some("ROUTER_API_KEY"), None));
    assert!(plan.changes.iter().all(|c| c.before.as_deref() != Some("sk-previous-token-WXYZ")));
}

#[test]
fn codex_invalid_toml_is_rejected_without_echoing_content() {
    let err = codex::plan(Some("secret = \"SECRET-LEAK-123\"\nbroken = [\n"), &params()).err().unwrap();
    assert!(err.contains("不是合法的 TOML") && err.contains("已拒绝合并"), "{err}");
    assert!(!err.contains("SECRET-LEAK-123"), "错误信息不得回显文件内容: {err}");
}

#[test]
fn codex_inline_model_providers_is_rejected() {
    let err = codex::plan(Some("model_providers = { a = { name = \"x\" } }\n"), &params()).err().unwrap();
    assert!(err.contains("model_providers"), "{err}");
}

// ───────── Claude Code ─────────

const CLAUDE_SAMPLE: &str = r#"{
  "permissions": { "allow": ["Bash(ls)"] },
  "env": { "FOO": "1", "ANTHROPIC_BASE_URL": "http://old", "ANTHROPIC_AUTH_TOKEN": "sk-previous-token-WXYZ" },
  "theme": "dark"
}"#;

fn claude_params() -> Params {
    validate(&input(ClientKind::Claude)).unwrap()
}

#[test]
fn claude_merge_keeps_other_settings_and_key_order() {
    let plan = claude::plan(Some(CLAUDE_SAMPLE), &claude_params()).unwrap();
    let v: Value = serde_json::from_str(&plan.content).unwrap();
    let top: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(top, ["permissions", "env", "theme"]);
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");
    assert_eq!(v["theme"], "dark");

    let env = v["env"].as_object().unwrap();
    let keys: Vec<&String> = env.keys().collect();
    assert_eq!(
        keys,
        ["FOO", "ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL"]
    );
    assert_eq!(env["FOO"], "1");
    assert_eq!(env["ANTHROPIC_BASE_URL"], ORIGIN);
    assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], KEY);
    for k in ["ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL"] {
        assert_eq!(env[k], "openai/gpt-4o-mini", "{k}");
    }
    assert!(plan.content.ends_with("}\n"));

    let base = plan.changes.iter().find(|c| c.field == "env.ANTHROPIC_BASE_URL").unwrap();
    assert_eq!((base.before.as_deref(), base.after.as_deref()), (Some("http://old"), Some(ORIGIN)));
    let token = plan.changes.iter().find(|c| c.field == "env.ANTHROPIC_AUTH_TOKEN").unwrap();
    assert_eq!(token.before.as_deref(), Some("sk-p…WXYZ"), "旧密钥打码");
    assert!(!plan.unchanged);
}

#[test]
fn claude_import_maps_each_model_tier_independently() {
    let mut request = input(ClientKind::Claude);
    request.haiku_model = Some("fast".into());
    request.sonnet_model = Some("balanced".into());
    request.opus_model = Some("strong".into());
    let plan = claude::plan(None, &validate(&request).unwrap()).unwrap();
    let config: Value = serde_json::from_str(&plan.content).unwrap();
    assert_eq!(config["env"]["ANTHROPIC_MODEL"], "openai/gpt-4o-mini");
    assert_eq!(config["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "fast");
    assert_eq!(config["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "balanced");
    assert_eq!(config["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "strong");
    request.haiku_model = Some(" ".into());
    assert!(validate(&request).is_err());
}

#[test]
fn claude_empty_or_missing_file_starts_from_empty_object() {
    for existing in [None, Some(""), Some("  \n\t")] {
        let plan = claude::plan(existing, &claude_params()).unwrap();
        let v: Value = serde_json::from_str(&plan.content).unwrap();
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
        assert_eq!(v.as_object().unwrap().len(), 1);
    }
}

#[test]
fn claude_unchanged_is_semantic_not_textual() {
    let first = claude::plan(Some(CLAUDE_SAMPLE), &claude_params()).unwrap();
    // 换成 4 空格缩进：内容没变，不应触发重写和备份
    let v: Value = serde_json::from_str(&first.content).unwrap();
    let mut four = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    serde::Serialize::serialize(&v, &mut serde_json::Serializer::with_formatter(&mut four, fmt)).unwrap();
    let second = claude::plan(Some(std::str::from_utf8(&four).unwrap()), &claude_params()).unwrap();
    assert!(second.unchanged);
}

#[test]
fn claude_rejects_unmergeable_files_without_echoing_content() {
    let err = claude::plan(Some("{\"a\": \"SECRET-LEAK-123\" ,"), &claude_params()).err().unwrap();
    assert!(err.contains("不是合法的 JSON") && err.contains("已拒绝合并"), "{err}");
    assert!(!err.contains("SECRET-LEAK-123"), "{err}");
    assert!(claude::plan(Some("[1,2]"), &claude_params()).err().unwrap().contains("顶层不是对象"));
    assert!(claude::plan(Some(r#"{"env": "oops"}"#), &claude_params()).err().unwrap().contains("env 不是对象"));
}

// ───────── 参数校验与路径 ─────────

#[test]
fn input_validation_and_origin_normalisation() {
    let ok = |origin: &str| {
        let mut i = input(ClientKind::Codex);
        i.origin = origin.into();
        validate(&i)
    };
    assert_eq!(ok("http://127.0.0.1:20128/").unwrap().origin, "http://127.0.0.1:20128");
    assert_eq!(ok(" http://[::1]:20128 ").unwrap().origin, "http://[::1]:20128");
    for bad in ["", "ftp://x:1", "http://x:1/v1", "http://x:1/?a=1", "not a url"] {
        assert!(ok(bad).is_err(), "{bad}");
    }
    let with = |f: fn(&mut ImportInput)| {
        let mut i = input(ClientKind::Codex);
        f(&mut i);
        validate(&i).is_err()
    };
    assert!(with(|i| i.api_key = "".into()));
    assert!(with(|i| i.api_key = "a\nb".into()));
    assert!(with(|i| i.model = "  ".into()));
    assert!(with(|i| i.model = "m\u{0}".into()));
    // 引号等特殊字符是合法的，交给 TOML / JSON 序列化器转义
    let mut i = input(ClientKind::Codex);
    i.api_key = "k\"ey\\x".into();
    let plan = codex::plan(None, &validate(&i).unwrap()).unwrap();
    assert_eq!(toml(&plan.content)["model_providers"]["freeapibox"]["experimental_bearer_token"].as_str(), Some("k\"ey\\x"));
}

#[test]
fn config_path_resolution() {
    let home = Path::new("/home/u");
    assert_eq!(resolve_path(ClientKind::Codex, home, None), Path::new("/home/u/.codex/config.toml"));
    assert_eq!(resolve_path(ClientKind::Claude, home, None), Path::new("/home/u/.claude/settings.json"));
    assert_eq!(resolve_path(ClientKind::Codex, home, Some("/data/codex")), Path::new("/data/codex/config.toml"));
    assert_eq!(resolve_path(ClientKind::Claude, home, Some(" /data/c ")), Path::new("/data/c/settings.json"));
    // 空值、相对路径都忽略
    assert_eq!(resolve_path(ClientKind::Codex, home, Some("")), Path::new("/home/u/.codex/config.toml"));
    assert_eq!(resolve_path(ClientKind::Codex, home, Some("rel/dir")), Path::new("/home/u/.codex/config.toml"));
}

// ───────── 文件读写 ─────────

#[test]
fn preview_reports_missing_file_and_errors_instead_of_failing() {
    let t = Tmp::new();
    let p = preview(&t.path(".codex/config.toml"), &input(ClientKind::Codex));
    assert!(!p.exists && !p.unchanged && p.error.is_none() && !p.changes.is_empty());

    // 路径是目录
    fs::create_dir_all(t.path("dir")).unwrap();
    assert!(preview(&t.path("dir"), &input(ClientKind::Codex)).error.unwrap().contains("目录"));

    // 文件损坏
    fs::write(t.path("bad.toml"), "x = [").unwrap();
    let bad = preview(&t.path("bad.toml"), &input(ClientKind::Codex));
    assert!(bad.exists && bad.error.is_some() && bad.changes.is_empty());

    // 参数无效
    let mut i = input(ClientKind::Codex);
    i.origin = "ftp://x".into();
    assert!(preview(&t.path("none.toml"), &i).error.is_some());
}

#[test]
fn apply_creates_missing_dirs_and_file_with_private_mode() {
    let t = Tmp::new();
    let path = t.path("nested/.codex/config.toml");
    let out = apply(&path, &input(ClientKind::Codex)).unwrap();
    assert!(out.changed && out.backup_path.is_none());
    assert_eq!(toml(&fs::read_to_string(&path).unwrap())["model_provider"].as_str(), Some("freeapibox"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "新文件含密钥，只给本人读写");
    }
    assert!(!path.with_file_name(".config.toml.router-tmp").exists());
}

#[test]
fn apply_backs_up_original_then_is_a_noop_the_second_time() {
    let t = Tmp::new();
    let path = t.path("config.toml");
    fs::write(&path, CODEX_SAMPLE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    let out = apply(&path, &input(ClientKind::Codex)).unwrap();
    assert!(out.changed);
    let bak = out.backup_path.expect("改动已有文件必须备份");
    assert_eq!(fs::read_to_string(&bak).unwrap(), CODEX_SAMPLE, "备份必须是原文件的逐字节副本");
    assert_eq!(toml(&fs::read_to_string(&path).unwrap())["model_providers"]["freeapibox"]["base_url"].as_str(), Some("http://127.0.0.1:20128/v1"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644, "沿用原文件的权限");
    }

    let again = apply(&path, &input(ClientKind::Codex)).unwrap();
    assert!(!again.changed && again.backup_path.is_none());
    assert_eq!(backups(&t.0).len(), 1, "没有变化时不产生新的备份");
}

#[test]
fn apply_refuses_to_overwrite_a_file_it_cannot_parse() {
    let t = Tmp::new();
    let path = t.path("settings.json");
    let broken = "{ not json // comment";
    fs::write(&path, broken).unwrap();
    let err = apply(&path, &input(ClientKind::Claude)).unwrap_err();
    assert!(err.contains("已拒绝合并"), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), broken, "解析失败时原文件必须原封不动");
    assert!(backups(&t.0).is_empty());
}

#[cfg(unix)]
#[test]
fn apply_writes_through_symlinks_and_keeps_the_link() {
    let t = Tmp::new();
    fs::create_dir_all(t.path("dotfiles")).unwrap();
    fs::create_dir_all(t.path("home/.claude")).unwrap();
    let real = t.path("dotfiles/settings.json");
    let link = t.path("home/.claude/settings.json");
    fs::write(&real, CLAUDE_SAMPLE).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let out = apply(&link, &input(ClientKind::Claude)).unwrap();
    assert!(out.changed);
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "符号链接不能被替换成普通文件");
    let v: Value = serde_json::from_str(&fs::read_to_string(&real).unwrap()).unwrap();
    assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
    assert_eq!(v["theme"], "dark");
    assert_eq!(backups(&t.path("dotfiles")).len(), 1, "备份放在真实文件旁边");
}
