//! 代理地址解析与校验：批量导入的每一行、手工录入的主机名。

use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use url::Url;

use super::Scheme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNode {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// 校验并规整主机名（去掉 IPv6 方括号）。拒绝带协议头、路径、空白的输入。
pub fn normalize_host(raw: &str) -> Result<String, String> {
    let host = raw.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    if host.is_empty() || host.len() > 253 {
        return Err("主机地址不能为空且不超过 253 个字符".into());
    }
    if host.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@' | '?' | '#')) {
        return Err("主机地址只能填写域名或 IP，不要带协议头、路径或账号".into());
    }
    let probe = if host.contains(':') { format!("http://[{host}]:1") } else { format!("http://{host}:1") };
    match Url::parse(&probe) {
        Ok(u) if u.host_str().is_some() => Ok(host.to_ascii_lowercase()),
        _ => Err("主机地址格式无效".into()),
    }
}

fn decode(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    Some(percent_decode_str(s).decode_utf8_lossy().into_owned())
}

/// 解析一行代理。支持：
/// `scheme://[user:pass@]host:port`、`[user:pass@]host:port`（默认 http）、`host:port`、`host:port:user:pass`。
pub fn parse_line(line: &str) -> Result<ParsedNode, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("空行".into());
    }
    let with_scheme = if line.contains("://") {
        line.to_string()
    } else if line.contains('@') {
        format!("http://{line}")
    } else {
        let parts: Vec<&str> = line.split(':').collect();
        match parts.len() {
            2 => format!("http://{line}"),
            4 => {
                let enc = |s: &str| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string();
                format!("http://{}:{}@{}:{}", enc(parts[2]), enc(parts[3]), parts[0], parts[1])
            }
            _ => return Err("格式无法识别（应为 host:port、host:port:用户:密码 或 scheme://用户:密码@host:port）".into()),
        }
    };
    // 用非特殊协议解析，避免 url 库把 http 的 :80 / https 的 :443 当成默认端口吞掉
    let (scheme_str, rest) = with_scheme.split_once("://").ok_or("地址格式无效")?;
    let url = Url::parse(&format!("socks5://{rest}")).map_err(|_| "地址格式无效".to_string())?;
    let scheme = match scheme_str.to_ascii_lowercase().as_str() {
        "http" => Scheme::Http,
        "https" => Scheme::Https,
        "socks5" | "socks" => Scheme::Socks5,
        "socks5h" => Scheme::Socks5h,
        other => return Err(format!("不支持的协议：{other}（支持 http / https / socks5 / socks5h）")),
    };
    let host = url.host_str().ok_or("缺少主机地址")?;
    let host = normalize_host(host)?;
    let port = url.port().ok_or("缺少端口")?;
    if !url.path().trim_matches('/').is_empty() || url.query().is_some() || url.fragment().is_some() {
        return Err("代理地址不应包含路径或参数".into());
    }
    Ok(ParsedNode {
        scheme,
        host,
        port,
        username: decode(url.username()),
        password: url.password().and_then(decode),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_formats() {
        let n = parse_line("socks5://alice:p%40ss@10.0.0.1:1080").unwrap();
        assert_eq!((n.scheme, n.host.as_str(), n.port), (Scheme::Socks5, "10.0.0.1", 1080));
        assert_eq!(n.username.as_deref(), Some("alice"));
        assert_eq!(n.password.as_deref(), Some("p@ss"));

        let n = parse_line("proxy.example.com:8080").unwrap();
        assert_eq!((n.scheme, n.host.as_str(), n.port, n.username), (Scheme::Http, "proxy.example.com", 8080, None));

        let n = parse_line("1.2.3.4:3128:bob:se:cret").unwrap_err();
        assert!(n.contains("格式"));

        let n = parse_line("1.2.3.4:3128:bob:secret").unwrap();
        assert_eq!((n.username.as_deref(), n.password.as_deref()), (Some("bob"), Some("secret")));

        let n = parse_line("user:pw@host.io:9").unwrap();
        assert_eq!((n.scheme, n.username.as_deref(), n.port), (Scheme::Http, Some("user"), 9));

        let n = parse_line("socks5h://[::1]:1080").unwrap();
        assert_eq!((n.scheme, n.host.as_str()), (Scheme::Socks5h, "::1"));
    }

    #[test]
    fn rejects_bad_lines() {
        for bad in ["", "host", "ftp://h:1", "http://h", "http://h:1/path", "a:b:c", "http://:80", "socks4://h:1"] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
        assert!(parse_line("http://h:70000").is_err());
    }

    #[test]
    fn host_validation() {
        assert_eq!(normalize_host(" Proxy.Example.com ").unwrap(), "proxy.example.com");
        assert_eq!(normalize_host("[2001:db8::1]").unwrap(), "2001:db8::1");
        for bad in ["", "a b", "http://x", "x/y", "u@x", "x?y"] {
            assert!(normalize_host(bad).is_err(), "{bad}");
        }
    }
}
