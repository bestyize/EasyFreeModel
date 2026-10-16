//! 出站 HTTP：统一 User-Agent、鉴权、响应大小限制，以及下载上游返回 URL 时的 SSRF 防护。
//!
//! 红线：
//! - User-Agent 固定为 `RouterDesktop/<版本>`，不伪造设备/会话标识；
//! - 不禁用 TLS 证书校验；
//! - API 请求不跟随重定向（避免凭据被带到别的主机）；
//! - 下载上游返回的 URL 只允许 https，且禁止指向回环/内网/链路本地等地址。域名要先解析再逐个检查，
//!   直连时固定到已检查的地址；经显式代理时由节点处理目标 DNS。每一跳重定向都重新检查。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, LOCATION};
use reqwest::{redirect, Client, Method, RequestBuilder, Response, Url};
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use super::adapter::{MediaError, Secret};
use super::registry::{AuthSpec, CapabilityConfig};

pub const API_TIMEOUT: Duration = Duration::from_secs(300);
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(900);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// API 的 JSON 响应上限（含 base64 图片）。
pub const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_REDIRECTS: usize = 5;
const MAX_ERROR_BODY: usize = 16 * 1024;
const MAX_ERROR_MESSAGE_CHARS: usize = 300;

/// 固定 UA：产品名 + 版本，不模仿任何其它客户端。
pub fn user_agent() -> String {
    format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION"))
}

// ---------------------------------------------------------------------------------------------
// URL 策略 / SSRF
// ---------------------------------------------------------------------------------------------

/// 下载/后续地址的 URL 策略。生产环境没有任何例外；测试可豁免假上游的地址。
#[derive(Debug, Clone, Default)]
pub struct UrlPolicy {
    #[cfg(test)]
    exempt: Vec<SocketAddr>,
}

impl UrlPolicy {
    #[cfg(test)]
    pub fn with_exempt(exempt: Vec<SocketAddr>) -> Self {
        UrlPolicy { exempt }
    }

    #[cfg(test)]
    fn exempt_addr(&self, url: &Url) -> Option<SocketAddr> {
        let host = url.host_str()?;
        let ip: IpAddr = host.trim_matches(['[', ']']).parse().ok()?;
        let port = url.port_or_known_default()?;
        self.exempt.iter().find(|a| a.ip() == ip && a.port() == port).copied()
    }

    #[cfg(not(test))]
    fn exempt_addr(&self, _url: &Url) -> Option<SocketAddr> {
        None
    }
}

/// 不允许作为下载目标的 IP：回环、私网、链路本地（含云元数据 169.254.169.254）、CGNAT、保留/文档/组播等。
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && (o[1] & 0xFE) == 18) // 198.18.0.0/15 基准测试
        || o[0] >= 240 // 240.0.0.0/4 保留
        || o[0] == 0
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_v4(v4);
    }
    let s = ip.segments();
    // 64:ff9b::/96 NAT64、2002::/16 6to4：内嵌 IPv4 同样检查
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|x| *x == 0) {
        return is_forbidden_v4(Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8));
    }
    if s[0] == 0x2002 {
        return is_forbidden_v4(Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8));
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xFE00) == 0xFC00 // fc00::/7 唯一本地
        || (s[0] & 0xFFC0) == 0xFE80 // fe80::/10 链路本地
        || (s[0] & 0xFFC0) == 0xFEC0 // fec0::/10 site-local（已废弃）
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 文档前缀
        || (s[0] == 0x0100 && s[1..4].iter().all(|x| *x == 0)) // 100::/64 discard
}

fn forbidden_hostname(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
        || h.ends_with(".lan")
        || h.ends_with(".home.arpa")
        || h.ends_with(".intranet")
        || h.ends_with(".corp")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// IP 字面量（已检查）或测试豁免的地址。
    Ip(SocketAddr),
    /// 域名：还需要解析并逐个检查解析结果。
    Domain { host: String, port: u16 },
}

#[derive(Debug, Clone)]
pub struct Checked {
    pub url: Url,
    pub target: Target,
}

/// 同步部分的 URL 检查（可单测，不触网）：只允许 https、不带用户名密码、不指向禁止的主机/IP。
pub fn check_url(raw: &str, policy: &UrlPolicy) -> Result<Checked, MediaError> {
    let url = Url::parse(raw.trim()).map_err(|_| MediaError::Blocked("URL 格式无效".into()))?;
    if let Some(addr) = policy.exempt_addr(&url) {
        return Ok(Checked { url, target: Target::Ip(addr) });
    }
    if url.scheme() != "https" {
        return Err(MediaError::Blocked("只允许 https 地址".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(MediaError::Blocked("地址中不允许包含用户名或密码".into()));
    }
    let port = url.port_or_known_default().unwrap_or(443);
    // Url 解析会把 `2130706433`、`0x7f.1` 之类的写法规范成点分十进制，所以这里按规范化后的主机串判断
    let host = url.host_str().ok_or_else(|| MediaError::Blocked("地址缺少主机名".into()))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(MediaError::Blocked("禁止访问回环/内网/保留地址".into()));
        }
        return Ok(Checked { target: Target::Ip(SocketAddr::new(ip, port)), url });
    }
    if forbidden_hostname(host) {
        return Err(MediaError::Blocked("禁止访问本机或内网主机名".into()));
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    Ok(Checked { url, target: Target::Domain { host, port } })
}

/// 检查域名的解析结果：只要有一个地址落在禁止范围就整体拒绝（防止混合解析绕过）。
pub fn check_resolved(addrs: &[SocketAddr]) -> Result<(), MediaError> {
    if addrs.is_empty() {
        return Err(MediaError::Blocked("域名无法解析".into()));
    }
    if addrs.iter().any(|a| is_forbidden_ip(a.ip())) {
        return Err(MediaError::Blocked("域名解析到回环/内网/保留地址".into()));
    }
    Ok(())
}

async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, MediaError> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| MediaError::Blocked("域名无法解析".into()))?
        .collect();
    check_resolved(&addrs)?;
    Ok(addrs)
}

/// 域名是否属于该供应商：baseUrl 的主机，或 `trustedHosts` 里的域名后缀。
pub fn host_trusted(host: &str, cfg: &CapabilityConfig) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Ok(base) = Url::parse(&cfg.base_url) {
        if base.host_str().map(|h| h.eq_ignore_ascii_case(&host)).unwrap_or(false) {
            return true;
        }
    }
    cfg.trusted_hosts.iter().any(|s| {
        let s = s.trim_start_matches('.').to_ascii_lowercase();
        host == s || host.ends_with(&format!(".{s}"))
    })
}

// ---------------------------------------------------------------------------------------------
// 错误转换
// ---------------------------------------------------------------------------------------------

/// reqwest 错误 → 不含 URL（URL 里可能有查询参数形式的密钥）的中文描述。
pub fn net_err(e: reqwest::Error) -> MediaError {
    if e.is_timeout() {
        return MediaError::Timeout;
    }
    let e = e.without_url();
    let kind = if e.is_connect() {
        "连接失败"
    } else if e.is_body() {
        "读取响应失败"
    } else if e.is_decode() {
        "响应解码失败"
    } else if e.is_redirect() {
        "重定向异常"
    } else {
        "请求失败"
    };
    let mut cause = String::new();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        if !cause.is_empty() {
            cause.push('：');
        }
        cause.push_str(&s.to_string());
        src = s.source();
    }
    if cause.is_empty() {
        MediaError::Network(kind.to_string())
    } else {
        MediaError::Network(format!("{kind}（{}）", clean_message(&cause)))
    }
}

/// 折叠空白/控制字符并截断。
pub fn clean_message(s: &str) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    super::adapter::util::truncate_chars(&flat, MAX_ERROR_MESSAGE_CHARS)
}

/// 从常见的错误 JSON 结构里取出可读的错误文本。
pub fn extract_error_message(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let pick = |v: &Value| -> Option<String> {
            match v {
                Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
                _ => None,
            }
        };
        let candidates: [Option<String>; 12] = [
            v.pointer("/error/message").and_then(pick),
            v.pointer("/error").and_then(pick),
            v.pointer("/message").and_then(pick),
            v.pointer("/detail/message").and_then(pick),
            v.pointer("/detail").and_then(pick),
            v.pointer("/detail/0/msg").and_then(pick),
            v.pointer("/errors/0").and_then(pick),
            v.pointer("/errors/0/message").and_then(pick),
            v.pointer("/msg").and_then(pick),
            v.pointer("/error_description").and_then(pick),
            v.pointer("/failure").and_then(pick),
            v.pointer("/error/detail").and_then(pick),
        ];
        if let Some(m) = candidates.into_iter().flatten().next() {
            return Some(clean_message(&m));
        }
    }
    let t = clean_message(&text);
    (!t.is_empty()).then_some(t)
}

pub async fn error_from_response(resp: Response) -> MediaError {
    let status = resp.status();
    if status.is_redirection() {
        return MediaError::Upstream { status: status.as_u16(), message: "上游返回重定向，已拒绝跟随".into() };
    }
    let body = read_prefix(resp, MAX_ERROR_BODY).await;
    let message = extract_error_message(&body)
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("请求失败").to_string());
    MediaError::Upstream { status: status.as_u16(), message }
}

async fn read_prefix(mut resp: Response, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let room = max.saturating_sub(out.len());
        out.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if out.len() >= max {
            break;
        }
    }
    out
}

pub fn human_bytes(n: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if n >= 1024 * MIB {
        format!("{} GB", n / (1024 * MIB))
    } else if n >= MIB {
        format!("{} MB", n / MIB)
    } else {
        format!("{} KB", n.div_ceil(1024))
    }
}

// ---------------------------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MediaHttp {
    client: Client,
    policy: UrlPolicy,
}

fn base_builder() -> reqwest::ClientBuilder {
    Client::builder().no_proxy().user_agent(user_agent()).redirect(redirect::Policy::none()).connect_timeout(CONNECT_TIMEOUT)
}

fn sensitive_header(name: &str, value: &str) -> Result<(HeaderName, HeaderValue), MediaError> {
    let n = HeaderName::from_bytes(name.as_bytes()).map_err(|_| MediaError::InvalidParams("供应商配置的请求头名无效".into()))?;
    // 报错时不回显值：值可能是密钥
    let mut v = HeaderValue::from_str(value).map_err(|_| MediaError::InvalidParams("API Key 含有不能放进请求头的字符".into()))?;
    v.set_sensitive(true);
    Ok((n, v))
}

impl MediaHttp {
    pub fn new() -> Result<Self, MediaError> {
        let client = base_builder()
            .timeout(API_TIMEOUT)
            .build()
            .map_err(|e| MediaError::Network(format!("初始化 HTTP 客户端失败：{}", clean_message(&e.without_url().to_string()))))?;
        Ok(MediaHttp { client, policy: UrlPolicy::default() })
    }

    /// 测试用：豁免假上游的地址（其余规则照常生效）。
    #[cfg(test)]
    pub fn for_test(exempt: Vec<SocketAddr>) -> Self {
        MediaHttp { client: base_builder().timeout(Duration::from_secs(30)).build().unwrap(), policy: UrlPolicy::with_exempt(exempt) }
    }

    pub fn policy(&self) -> &UrlPolicy {
        &self.policy
    }

    /// 构造带鉴权与固定头的请求。查询参数形式的鉴权在这里并入 URL。
    pub fn request(&self, method: Method, url: &str, cfg: &CapabilityConfig, secret: &Secret) -> Result<RequestBuilder, MediaError> {
        let mut u = Url::parse(url).map_err(|_| MediaError::InvalidParams("供应商 baseUrl 无效".into()))?;
        if let AuthSpec::Query { name } = &cfg.auth {
            u.query_pairs_mut().append_pair(name, secret.expose());
        }
        let mut rb = self.client.request(method, u);
        for (k, v) in &cfg.extra_headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        Self::apply_header_auth(rb, &cfg.auth, secret)
    }

    fn apply_header_auth(rb: RequestBuilder, auth: &AuthSpec, secret: &Secret) -> Result<RequestBuilder, MediaError> {
        Ok(match auth {
            AuthSpec::Bearer => {
                let (n, v) = sensitive_header(AUTHORIZATION.as_str(), &format!("Bearer {}", secret.expose()))?;
                rb.header(n, v)
            }
            AuthSpec::Header { name, prefix } => {
                let (n, v) = sensitive_header(name, &format!("{prefix}{}", secret.expose()))?;
                rb.header(n, v)
            }
            AuthSpec::Query { .. } | AuthSpec::None => rb,
        })
    }

    /// 发送并把非 2xx 转成 `MediaError::Upstream`（带脱敏后的上游错误摘要）。
    pub async fn send(&self, provider_id: &str, rb: RequestBuilder) -> Result<Response, MediaError> {
        let resp = crate::proxy::http::send(provider_id, rb, API_TIMEOUT).await
            .map_err(|e| MediaError::Network(e.to_string()))?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(error_from_response(resp).await)
        }
    }

    pub async fn send_json(&self, provider_id: &str, rb: RequestBuilder) -> Result<Value, MediaError> {
        let resp = self.send(provider_id, rb).await?;
        read_json(resp, MAX_JSON_BYTES).await
    }

    /// 校验上游响应里带回的后续地址（轮询/取消地址）：它们会带着凭据被访问，必须在受信域名内。
    pub fn check_trusted_url(&self, raw: &str, cfg: &CapabilityConfig) -> Result<Url, MediaError> {
        let url = Url::parse(raw.trim()).map_err(|_| MediaError::Blocked("上游返回的地址无效".into()))?;
        if self.policy.exempt_addr(&url).is_some() {
            return Ok(url);
        }
        if url.scheme() != "https" {
            return Err(MediaError::Blocked("上游返回的地址不是 https".into()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(MediaError::Blocked("地址中不允许包含用户名或密码".into()));
        }
        match url.host_str() {
            Some(h) if host_trusted(h, cfg) => Ok(url),
            _ => Err(MediaError::Blocked("上游返回的地址不在该供应商的受信域名内".into())),
        }
    }

    /// 下载上游返回的 URL 到 `dest`（先写 `.part` 再改名）。
    ///
    /// - `creds` 仅当目标与首个 URL 同主机且属于受信域名时才带上凭据（Bearer/Header 方式）；
    /// - 每一跳（含重定向）都重新做 SSRF 检查；直连连接固定到检查过的地址；
    /// - 超过 `max_bytes` 立即中止并删除半成品。
    pub async fn download_to_file(
        &self,
        provider_id: &str,
        url: &str,
        creds: Option<(&CapabilityConfig, &Secret)>,
        max_bytes: u64,
        dest: &Path,
    ) -> Result<u64, MediaError> {
        let origin_host = Url::parse(url.trim()).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase));
        let mut current = url.trim().to_string();
        for _ in 0..=MAX_REDIRECTS {
            let checked = check_url(&current, &self.policy)?;
            let (builder, host_for_auth) = match &checked.target {
                Target::Ip(_) => (base_builder(), checked.url.host_str().map(str::to_ascii_lowercase)),
                Target::Domain { host, port } => {
                    let addrs = resolve_public(host, *port).await?;
                    (base_builder().resolve_to_addrs(host, &addrs), Some(host.clone()))
                }
            };
            let client = builder
                .timeout(DOWNLOAD_TIMEOUT)
                .build()
                .map_err(|e| MediaError::Network(clean_message(&e.without_url().to_string())))?;
            let mut rb = client.request(Method::GET, checked.url.clone()).header(ACCEPT, "*/*");
            if let (Some((cfg, secret)), Some(h)) = (creds, host_for_auth.as_deref()) {
                let same_origin = origin_host.as_deref() == Some(h);
                if same_origin && host_trusted(h, cfg) {
                    rb = Self::apply_header_auth(rb, &cfg.auth, secret)?;
                }
            }
            let resp = crate::proxy::http::send(provider_id, rb, DOWNLOAD_TIMEOUT).await
                .map_err(|e| MediaError::Network(e.to_string()))?;
            if resp.status().is_redirection() {
                let loc = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| MediaError::Blocked("重定向缺少 Location".into()))?;
                current = checked.url.join(loc).map_err(|_| MediaError::Blocked("重定向地址无效".into()))?.to_string();
                continue;
            }
            if !resp.status().is_success() {
                return Err(error_from_response(resp).await);
            }
            return stream_to_file(resp, max_bytes, dest).await;
        }
        Err(MediaError::Blocked("重定向次数过多".into()))
    }
}

/// 把响应体流式写入文件，边写边校验大小。
pub async fn stream_to_file(mut resp: Response, max_bytes: u64, dest: &Path) -> Result<u64, MediaError> {
    if resp.content_length().is_some_and(|n| n > max_bytes) {
        return Err(MediaError::TooLarge(human_bytes(max_bytes)));
    }
    let part = dest.with_extension(match dest.extension().and_then(|e| e.to_str()) {
        Some(e) => format!("{e}.part"),
        None => "part".to_string(),
    });
    let result = async {
        let mut file = tokio::fs::File::create(&part).await?;
        let mut total: u64 = 0;
        while let Some(chunk) = resp.chunk().await.map_err(net_err)? {
            total += chunk.len() as u64;
            if total > max_bytes {
                return Err(MediaError::TooLarge(human_bytes(max_bytes)));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        if total == 0 {
            return Err(MediaError::BadResponse("下载内容为空".into()));
        }
        tokio::fs::rename(&part, dest).await?;
        Ok(total)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&part).await;
    }
    result
}

/// 读取响应体到内存，超过上限就报错。
pub async fn read_limited(mut resp: Response, max: u64) -> Result<Vec<u8>, MediaError> {
    if resp.content_length().is_some_and(|n| n > max) {
        return Err(MediaError::TooLarge(human_bytes(max)));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(net_err)? {
        if out.len() as u64 + chunk.len() as u64 > max {
            return Err(MediaError::TooLarge(human_bytes(max)));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

pub async fn read_json(resp: Response, max: u64) -> Result<Value, MediaError> {
    let bytes = read_limited(resp, max).await?;
    serde_json::from_slice(&bytes).map_err(|_| MediaError::BadResponse("响应不是有效的 JSON".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::testutil::{tempdir, Fake, Reply};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn user_agent_is_product_name_and_version_only() {
        let ua = user_agent();
        assert_eq!(ua, format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION")));
        assert!(!ua.contains("Mozilla"));
    }

    #[test]
    fn forbidden_ips_cover_loopback_private_and_metadata() {
        for s in [
            "127.0.0.1", "127.8.9.10", "10.0.0.1", "172.16.0.1", "172.31.255.255", "192.168.1.1", "169.254.169.254", "169.254.0.1",
            "100.64.0.1", "100.127.255.255", "0.0.0.0", "255.255.255.255", "224.0.0.1", "192.0.2.1", "198.18.0.1", "240.0.0.1",
            "::1", "::", "fe80::1", "fc00::1", "fd12:3456::1", "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:169.254.169.254",
            "64:ff9b::7f00:1", "2002:7f00:1::1", "2001:db8::1", "fec0::1",
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} 应被拦截");
        }
        for s in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "172.32.0.1", "100.63.255.255", "2606:4700:4700::1111", "::ffff:8.8.8.8"] {
            assert!(!is_forbidden_ip(ip(s)), "{s} 应放行");
        }
    }

    #[test]
    fn check_url_rejects_http_credentials_and_internal_hosts() {
        let p = UrlPolicy::default();
        let blocked = |u: &str| matches!(check_url(u, &p), Err(MediaError::Blocked(_)));
        assert!(blocked("http://example.com/a.png"), "http");
        assert!(blocked("ftp://example.com/a.png"));
        assert!(blocked("file:///etc/passwd"));
        assert!(blocked("data:text/plain;base64,AAAA"));
        assert!(blocked("https://127.0.0.1/a.png"));
        assert!(blocked("https://127.0.0.1:8443/a.png"));
        assert!(blocked("https://[::1]/a.png"));
        assert!(blocked("https://[::ffff:127.0.0.1]/a.png"));
        assert!(blocked("https://10.1.2.3/a.png"));
        assert!(blocked("https://192.168.0.10/a.png"));
        assert!(blocked("https://169.254.169.254/latest/meta-data/"));
        assert!(blocked("https://localhost/a.png"));
        assert!(blocked("https://LOCALHOST./a.png"));
        assert!(blocked("https://foo.localhost/a.png"));
        assert!(blocked("https://printer.local/a.png"));
        assert!(blocked("https://db.internal/a.png"));
        assert!(blocked("https://user:pass@example.com/a.png"));
        assert!(blocked("https://2130706433/a.png"), "十进制 IP 写法会被 URL 解析成 127.0.0.1");
        assert!(blocked("https://0x7f.0.0.1/a.png"));
        assert!(blocked("not a url"));
        assert!(blocked(""));

        let ok = check_url("https://cdn.example.com/a/b.png?sig=1", &p).unwrap();
        assert_eq!(ok.target, Target::Domain { host: "cdn.example.com".into(), port: 443 });
        let ok_ip = check_url("https://8.8.8.8:8443/x", &p).unwrap();
        assert_eq!(ok_ip.target, Target::Ip("8.8.8.8:8443".parse().unwrap()));
    }

    #[test]
    fn resolved_addresses_are_all_checked() {
        let a = |s: &str| -> SocketAddr { s.parse().unwrap() };
        assert!(check_resolved(&[a("8.8.8.8:443"), a("1.1.1.1:443")]).is_ok());
        assert!(check_resolved(&[a("8.8.8.8:443"), a("127.0.0.1:443")]).is_err(), "混合解析必须整体拒绝");
        assert!(check_resolved(&[a("[::1]:443")]).is_err());
        assert!(check_resolved(&[]).is_err());
    }

    #[test]
    fn trusted_hosts_match_base_and_suffixes_only() {
        let cfg = CapabilityConfig {
            base_url: "https://api.bfl.ai/v1".into(),
            trusted_hosts: vec!["bfl.ai".into(), ".fal.run".into()],
            ..Default::default()
        };
        assert!(host_trusted("api.bfl.ai", &cfg));
        assert!(host_trusted("api.eu1.bfl.ai", &cfg));
        assert!(host_trusted("bfl.ai", &cfg));
        assert!(host_trusted("queue.fal.run", &cfg));
        assert!(!host_trusted("evilbfl.ai", &cfg), "后缀必须在点边界上匹配");
        assert!(!host_trusted("bfl.ai.evil.com", &cfg));
        assert!(!host_trusted("example.com", &cfg));
    }

    #[test]
    fn trusted_url_requires_https_and_trusted_host() {
        let http = MediaHttp::new().unwrap();
        let cfg = CapabilityConfig { base_url: "https://api.bfl.ai/v1".into(), trusted_hosts: vec!["bfl.ai".into()], ..Default::default() };
        assert!(http.check_trusted_url("https://api.eu1.bfl.ai/v1/get_result?id=1", &cfg).is_ok());
        for bad in [
            "http://api.bfl.ai/v1/get_result",
            "https://evil.example.com/steal",
            "https://127.0.0.1/x",
            "https://user:pw@api.bfl.ai/x",
            "javascript:alert(1)",
        ] {
            assert!(matches!(http.check_trusted_url(bad, &cfg), Err(MediaError::Blocked(_))), "{bad}");
        }
    }

    #[test]
    fn extract_error_message_handles_common_shapes() {
        let m = |s: &str| extract_error_message(s.as_bytes()).unwrap();
        assert_eq!(m(r#"{"error":{"message":"bad key","type":"x"}}"#), "bad key");
        assert_eq!(m(r#"{"error":"nope"}"#), "nope");
        assert_eq!(m(r#"{"detail":{"status":"quota","message":"out of credits"}}"#), "out of credits");
        assert_eq!(m(r#"{"detail":"plain"}"#), "plain");
        assert_eq!(m(r#"{"errors":["a","b"]}"#), "a");
        assert_eq!(m("<html>\n  gateway\ttimeout </html>"), "<html> gateway timeout </html>");
        assert!(m(&"x".repeat(5000)).chars().count() <= MAX_ERROR_MESSAGE_CHARS + 1);
        assert!(extract_error_message(b"").is_none());
    }

    #[tokio::test]
    async fn download_writes_file_and_sends_fixed_user_agent() {
        let fake = Fake::start(|_r| Reply::bytes(200, "image/png", vec![1, 2, 3, 4])).await;
        let http = fake.http();
        let dir = tempdir();
        let dest = dir.join("a.png");
        let n = http.download_to_file("test", &format!("{}/img.png", fake.url()), None, 1024, &dest).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(std::fs::read(&dest).unwrap(), vec![1, 2, 3, 4]);
        assert!(!dir.join("a.png.part").exists());
        let req = &fake.requests()[0];
        assert_eq!(req.header("user-agent").unwrap(), user_agent());
        assert!(req.header("authorization").is_none(), "未传凭据不得带 Authorization");
    }

    #[tokio::test]
    async fn download_enforces_size_limit_and_cleans_up() {
        let fake = Fake::start(|_r| Reply::bytes(200, "video/mp4", vec![7u8; 4096])).await;
        let http = fake.http();
        let dir = tempdir();
        let dest = dir.join("v.mp4");
        let err = http.download_to_file("test", &format!("{}/v", fake.url()), None, 1000, &dest).await.unwrap_err();
        assert!(matches!(err, MediaError::TooLarge(_)), "{err:?}");
        assert!(!dest.exists());
        assert!(!dir.join("v.mp4.part").exists());
    }

    #[tokio::test]
    async fn download_blocks_redirect_to_internal_addresses() {
        for target in [
            "http://169.254.169.254/latest/meta-data/",
            "https://10.0.0.5/secret",
            "https://127.0.0.1:1/x",
            "http://127.0.0.1:1/x", // 回环但不是被豁免的假上游端口
            "https://localhost/x",
            "file:///etc/passwd",
        ] {
            let target = target.to_string();
            let fake = Fake::start(move |r| {
                if r.path.starts_with("/hop") {
                    Reply::redirect(&target)
                } else {
                    Reply::bytes(200, "text/plain", b"should not be reached".to_vec())
                }
            })
            .await;
            let http = fake.http();
            let tmp = tempdir();
            let dest = tmp.join("x.bin");
            let err = http.download_to_file("test", &format!("{}/hop", fake.url()), None, 1024, &dest).await.unwrap_err();
            assert!(matches!(err, MediaError::Blocked(_)), "redirect to {err:?}");
            assert!(!dest.exists());
            assert_eq!(fake.requests().len(), 1, "不应向内网目标发出请求");
        }
    }

    #[tokio::test]
    async fn download_follows_safe_redirect_and_limits_hops() {
        let fake = Fake::start(|r| match r.path.as_str() {
            "/a" => Reply::redirect("/b"),
            "/b" => Reply::bytes(200, "audio/mpeg", vec![9; 8]),
            _ => Reply::redirect("/loop"),
        })
        .await;
        let http = fake.http();
        let dir = tempdir();
        assert_eq!(http.download_to_file("test", &format!("{}/a", fake.url()), None, 100, &dir.join("ok.mp3")).await.unwrap(), 8);
        let err = http.download_to_file("test", &format!("{}/loop", fake.url()), None, 100, &dir.join("no.mp3")).await.unwrap_err();
        assert!(matches!(err, MediaError::Blocked(_)), "{err:?}");
    }

    #[tokio::test]
    async fn production_policy_refuses_loopback_download_even_for_plain_fake() {
        let fake = Fake::start(|_r| Reply::bytes(200, "text/plain", b"x".to_vec())).await;
        let http = MediaHttp::new().unwrap(); // 无豁免
        let err = http.download_to_file("test", &format!("{}/x", fake.url()), None, 100, &tempdir().join("x")).await.unwrap_err();
        assert!(matches!(err, MediaError::Blocked(_)), "{err:?}");
        assert!(fake.requests().is_empty());
    }

    #[tokio::test]
    async fn credentials_are_only_sent_to_the_original_trusted_host() {
        let secret = Secret::new("sk-test-download-secret");
        let fake = Fake::start(|_r| Reply::bytes(200, "video/mp4", vec![1; 4])).await;
        let http = fake.http();
        let cfg = CapabilityConfig { base_url: fake.url(), ..Default::default() };
        let dir = tempdir();
        http.download_to_file("test", &format!("{}/v.mp4", fake.url()), Some((&cfg, &secret)), 100, &dir.join("a.mp4")).await.unwrap();
        assert_eq!(fake.requests()[0].header("authorization").unwrap(), "Bearer sk-test-download-secret");

        // 不属于该供应商的域名：不带凭据
        let other = CapabilityConfig { base_url: "https://api.other.example".into(), ..Default::default() };
        http.download_to_file("test", &format!("{}/v.mp4", fake.url()), Some((&other, &secret)), 100, &dir.join("b.mp4")).await.unwrap();
        assert!(fake.requests()[1].header("authorization").is_none());
    }

    #[tokio::test]
    async fn api_requests_do_not_follow_redirects_and_redact_url_in_errors() {
        let secret = Secret::new("sk-query-secret-123");
        let fake = Fake::start(|_r| Reply::redirect("http://127.0.0.1:9/elsewhere")).await;
        let http = fake.http();
        let cfg = CapabilityConfig { base_url: fake.url(), auth: AuthSpec::Query { name: "api_key".into() }, ..Default::default() };
        let rb = http.request(Method::GET, &format!("{}/search", fake.url()), &cfg, &secret).unwrap();
        let err = http.send("test", rb).await.unwrap_err();
        assert!(matches!(err, MediaError::Upstream { status: 302, .. }), "{err:?}");
        assert_eq!(fake.requests().len(), 1);
        assert_eq!(fake.requests()[0].query("api_key").as_deref(), Some("sk-query-secret-123"));

        // 连接失败的错误文本不应包含带密钥的 URL
        let rb = http.request(Method::GET, "http://127.0.0.1:1/x", &cfg, &secret).unwrap();
        let err = http.send("test", rb).await.unwrap_err().to_string();
        assert!(!err.contains("sk-query-secret-123") && !err.contains("api_key"), "{err}");
    }

    #[test]
    fn invalid_key_characters_do_not_echo_the_key() {
        let http = MediaHttp::new().unwrap();
        let bad = Secret::new("sk-bad\nkey-value");
        let cfg = CapabilityConfig { base_url: "https://x.example".into(), ..Default::default() };
        let err = http.request(Method::GET, "https://x.example/a", &cfg, &bad).unwrap_err().to_string();
        assert!(!err.contains("sk-bad") && !err.contains("key-value"), "{err}");
    }
}
