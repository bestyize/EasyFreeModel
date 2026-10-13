//! 授权之后、转发之前的「令牌兑换」步骤的接入状态。
//!
//! github（账户令牌 -> Copilot 会话令牌）与 muse（账户令牌 -> 铸造 Model API Key）在取得账户令牌之后还需要
//! 一步兑换，授权框架不负责，本版本也没有接入。这两家的 OAuth 连接可以授权成功并保存令牌，但转发时必须如实
//! 报告该步骤尚未接入——绝不能把账户令牌当作模型调用的 Key 发给上游。

/// 面向用户/客户端的固定说明前缀（测试与界面据此断言）。
pub const TOKEN_EXCHANGE_PENDING: &str = "该提供商授权后的令牌兑换步骤尚未接入";

/// 该提供商授权后是否还需要尚未接入的令牌兑换步骤；有则返回完整说明。
pub fn token_exchange_note(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "github" => Some(
            "该提供商授权后的令牌兑换步骤尚未接入（GitHub：账户令牌需再换取 Copilot 会话令牌），授权成功的连接暂不能用于转发",
        ),
        "muse" => Some(
            "该提供商授权后的令牌兑换步骤尚未接入（Muse：账户令牌需再铸造 Model API Key），授权成功的连接暂不能用于转发",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_and_muse_need_the_missing_exchange_step() {
        for id in ["github", "muse"] {
            assert!(token_exchange_note(id).unwrap().starts_with(TOKEN_EXCHANGE_PENDING), "{id}");
        }
        for id in ["claude", "codex", "antigravity", "gemini-cli", "gitlab", "grok-cli", "kimi", "minimax-code"] {
            assert!(token_exchange_note(id).is_none(), "{id}");
        }
    }
}
