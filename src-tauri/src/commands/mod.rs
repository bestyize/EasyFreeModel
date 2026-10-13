//! Tauri 命令层。命令名与参数名必须与 `src/api/tauri.ts` 的 invoke 调用逐一对应
//! （Rust snake_case 参数由 Tauri 自动对应前端 camelCase）。命令内不直接写 SQL。

pub mod api_keys;
pub mod media;
pub mod oauth;
pub mod providers;
pub mod quota;
pub mod system;
pub mod usage;
