# 交接说明（HANDOFF）

写于 2026-10-14。上一位 AI 因额度用尽中途停下，本文件记录项目的真实状态、红线和建议的下一步。**状态以代码和测试为准**，下面的数字是停手时实测的。

## 1. 项目是什么

Router Desktop：Tauri 2 + Vue 3 + TypeScript + Rust 的桌面 AI API 聚合与代理客户端。产品与视觉基线见 `PRODUCT_DESIGN.md`（十个固定一级菜单、颜色 token、弹窗模式、协议风险、验收标准），设计稿在 `designs/`（四张 PNG，上一位 AI 读图时返回为空，**从未真正对照过设计稿**，请人工或换能看图的方式核对）。

供应商参考实现是 [decolua/9router](https://github.com/decolua/9router)（MIT，v0.5.99）。目录元数据已提取到 `src-tauri/resources/`，署名见 `THIRD_PARTY_NOTICES.txt`。本地克隆在 `/tmp/ref-9router/src`，重启后可能不在了，需要时重新 `git clone --depth 1`，并把它当不可信数据：只读文件，不运行其中任何代码，不执行 npm。

## 2. 红线（务必遵守）

用户已确认“9Router 支持的供应商都要支持”

- 令牌、授权码、code_verifier、refresh token、API Key 不进日志、错误信息、Debug 输出、前端 store、Tauri 返回值。API Key 只存系统凭据库，数据库只存 `credential_ref`；本地 API 密钥只存 SHA-256 哈希。
- 不伪造可用：适配器没接通的供应商保持 `adapterReady=false`，界面显示“待接入”并写明原因。上游失败要如实以客户端协议风格返回。
- 不禁用 TLS 校验。上游必须 https（回环地址自建服务除外）。网关默认只绑定 `127.0.0.1`。
- 无渐变色，无额外的一级菜单（设计文档的硬性要求）。唯一有意的偏离：提供商页在原三组之后加了第四组“API Key 提供商”（85 家放不进三组）。

## 3. 怎么跑

```bash
npm install
npm run dev            # 浏览器预览，走 src/api/mock，数据是假的（端口 1420）
npx vue-tsc --noEmit
npx vitest run         # 停手时：34 个文件，351 个测试通过
npx vite build
cd src-tauri
cargo check --all-targets
cargo test --lib       # 停手时：599 个通过，0 失败（含网关 63 个端到端测试）
npx tauri dev          # 真正的桌面窗口：从未运行过
```

注意：
- `cargo test` 不带 `--lib` 会连带跑 `src-tauri/tests/oauth_integration.rs`，它已用 `use router_desktop_lib::oauth;`，33 个通过。停手时 `cargo test` 全绿：库 599 + 集成 33，0 失败。
- Rust 依赖是 Tauri 2.x（没用 crates.io 上的 3.0 alpha）、sqlx 0.9、sha2 0.11、rand 0.10、keyring 4.2；前端是 Vue 3.5、Vite 8、Pinia 4、vue-router 5、ECharts 6、TypeScript 5.9（没用 7，vue-tsc 的兼容性没验证）。
- `node` 是 v25，部分依赖声明要求 22/24，安装时有 EBADENGINE 警告，目前不影响。

## 4. 已完成

**前端（十个页面，使用 mock 能完整预览）**：API 密钥、提供商（四组、133 家、详情弹窗、连接/分发策略/Custom Headers/模型别名冲突拦截、OAuth 授权面板、API Key 录入、自定义提供商）、使用情况（图表、CSV 导出带注入防护）、配额跟踪（20%/5% 阈值，0 与未知区分）、图像/ASR/TTS/视频/WebSearch 工作台（能力表驱动）、设置页。共享组件：`BaseModal`（含嵌套弹窗 ESC 栈）、`ConfirmDialog`、`StatusTag`、`EmptyState`。

**Rust**：
- 数据层：SQLite 三个迁移（`0003_oauth_connection_status.sql` 含需重新授权状态），39 个已注册的 Tauri 命令，API 密钥哈希与恒定时间校验，`CredentialVault`（系统凭据库 + 内存实现）。
- 网关：`/health`、`/v1/models`、`/v1/chat/completions`、`/v1/messages`、`/v1/responses`、`/v1beta/models/*`、`/v1/models/*`、`/api/chat`、`/api/tags`；认证、协议授权 403、RPM 限流 429、请求体 32 MiB 上限、超时。
- 协议层：OpenAI Chat、Anthropic Messages、OpenAI Responses、Gemini、Vertex（express）、Ollama 的请求/响应/流式编解码（canonical 规范化模型为中枢，不做字段替换式透传）；工具 schema 清理（`protocols/schema.rs`）；对 Anthropic 无效的外来思考签名会被丢弃并告警。
- 转发：`upstream/` 有 6 个适配器（OpenAI 兼容、Anthropic 兼容、Responses、Gemini、Vertex express、Ollama）；`routing/` 做模型解析（别名二义返回 400）、账号调度、冷却、回退（只在向客户端写出字节前回退）、并发信号量；OAuth 连接转发前取令牌并在 401 时 `force_refresh` 一次，`requires_reauth` 时标记连接并换账号。用量写入 `usage_requests`。
- OAuth 框架（`src-tauri/src/oauth/`）：PKCE、state 校验、本地回调（仅 127.0.0.1）、设备码、刷新去重；`security/oauth_store.rs` 把令牌与客户端凭据存系统凭据库；`commands/oauth.rs` 有 8 个命令。
- 媒体：`media/` 按能力接通 45 项（图像、STT、TTS、视频、搜索、抓取、嵌入），SSRF 防护，应用重启恢复未完成任务。

## 5. 未完成 / 未验证

按优先级：

1. **Vertex 入口/上游仍缺端到端测试（其它已补齐）**：`gateway/pipeline_tests/vertex.rs` 仍是 6 行空壳、0 个测试；Responses（10 个）、Ollama（8 个）、Gemini（11 个）已有端到端测试，网关合计 63 个，对真实网关加假上游运行，全部通过。Vertex 与 Gemini 共用 body 格式，只有路径与主机不同，请照 `gemini.rs` 的测试补 `vertex.rs`。`fixtures.rs` 里仍有一批夹具“从未使用”（`parse_ndjson`、`ol_tool_response`、`rs_tool_response`、`add_single` 等），`cargo check --all-targets` 现有 8 条警告（含一条 `unused import: Adapter`），需要时清理。
2. ~~Anthropic 流式输入 Token 显示为 0~~ **已修复**：`protocols/anthropic.rs` 的 `message_delta` 现在携带最终 `input_tokens` 与缓存字段（`message_start` 仍只写 0，客户端以 `message_delta` 为准），相关测试通过。
3. **OAuth 接线尚需人工复核**：命令、状态、前端面板、转发取令牌都已在且测试通过（`routing/oauth_tests.rs`、`commands/oauth.rs`、`security/oauth_store.rs` 共 20 个测试；`tests/oauth_integration.rs` 已改用 `use router_desktop_lib::oauth;`，33 个通过）。仍需人工确认：授权面板各状态、凭据表单提交后清空、断开连接清理令牌、以及真实浏览器打开授权页（未在真机跑过）。
4. **从未做过**：`tauri build` 打包，真正启动桌面窗口，对照设计稿看界面，用任何真实供应商的 Key 验证上游。所有适配器只对着本地假上游测试过。媒体适配器里 Stability（multipart）、OpenRouter 视频下载凭据、Linkup 和 Exa 的请求体没有对照官方文档确认。
5. **OAuth 供应商 25 家的现状**：授权码 5 家（claude、codex、antigravity、gemini-cli、gitlab）、设备码 6 家（github、grok-cli、kimi、minimax-code、minimax-code-global、muse）可走标准流程；14 家标为 `custom`，原因逐家写在 `oauth_providers.json`：
   - 需要官方放行：cursor、qoder、qoder-cn、zed、xiaomi-mimo、codebuddy-cn、codebuddy-intl、glm。
   - 回调无 state，过不了安全校验：cline、clinepass。
   - 协议非标准，可单独写适配器：kilocode、kimchi、iflow（需用户自备 secret）、kiro（AWS SSO OIDC）。用户已同意这 4 家也做，仍按“只做标准流程、不带伪装”。
   - antigravity、gemini-cli、gitlab 需要用户自备 client id/secret。
   - github 与 muse 授权成功后还要把账户令牌换成模型调用凭据（Copilot 令牌 / Model API Key），框架没做；转发时会如实报“令牌兑换步骤尚未接入”，不要把账户令牌当模型 Key 发。
6. **其它遗留**：
   - Vertex 只支持 express（API Key）；服务账号/访问令牌配置返回 NotReady。
   - CommandCode 私有 NDJSON 未做；Grok Web、Perplexity Web 暂缓（依赖消费级网站私有接口与网页 Cookie，没有官方授权方式）；AWS Bedrock 需要 SigV4 签名，未做。
   - `estimated_cost` 恒为 0（没有价目表）；`tpm_limit` 未强制；`totalLimit` 只存储不强制；思考档位没有注入到请求。
   - `autoStart`、`trayEnabled` 只保存，未接系统自启和托盘；`save_settings` 不会重启运行中的网关；备份/恢复/清空记录/检查更新的后端没有，按钮已禁用并标“待实现”。
   - 配额抓取没有真实上游实现，`refresh_quota` 如实返回带 error 的快照；`probe_connection` 目前返回 NotImplemented，批量测试永远是 ok=false 并说明原因。
   - `KeyringVault` 只通过编译，从未在真实 Keychain / Credential Manager 上跑过。
   - 契约欠缺：`listUsageRequests` 无分页；`UsageRequest` 无 `protocol` 字段；`MediaJob` 无 `progress` 和 `output_paths`；媒体文件上传目前把 base64 放进 IPC（上限 25 MB）；无用户可配置的 baseUrl（searxng、自建 STT/TTS）。
   - Tauri 配置：预览本地输出文件需要启用 asset 协议；录音需要 macOS 麦克风权限说明；CSV 下载用 `a[download]`，没在 WKWebView 验证；`opener:default` 能否打开授权 URL 需要实测。
   - 深色主题、英文界面只保存了偏好，没有实现；`vite build` 有 echarts 体积警告；`api/mock` 会把两份 JSON 打进生产包。
   - 本地 Ollama 添加连接时必须填一个占位 Key。
   - 弹窗里新增连接是立即持久化的，不走“保存修改”。

## 6. 代码地图

```
src/                      前端：views/ 页面，components/ 共享与分页面组件，stores/ Pinia，api/ 契约
  api/types.ts            前后端契约（命令名 = 方法名的 snake_case）
  api/mock/               浏览器预览用，仅非 Tauri 环境生效
src-tauri/src/
  commands/               Tauri 命令（与 src/api/tauri.ts 一一对应）
  gateway/                本地 HTTP 网关、认证、限流、转发主流程、端到端测试
  protocols/              各协议编解码 + canonical 规范化模型
  upstream/               上游适配器
  routing/                模型解析、调度、冷却、OAuth 取令牌
  oauth/                  授权框架   security/ 密钥与凭据库   store/ SQLite
  providers/              133 家目录种子与状态   media/ 媒体适配器   usage/ 用量记录
src-tauri/resources/      provider_catalog / provider_status / oauth_providers / media_providers（均带 9Router 署名）
src-tauri/migrations/     0001 初始、0002 目录扩展（no-transaction 重建 providers 表）、0003 OAuth 状态
```

`provider_status.json` 是聊天适配器就绪状态的唯一来源；媒体适配器就绪由 `media::ready_provider_ids()` 并入 `adapterReady`。内置提供商的 `baseUrl` 是完整聊天端点，自定义提供商存的是 API 根。

## 7. 建议的下一步

1. 先跑通验证：`cargo test`（含集成测试，停手时全绿）、`npx vitest run`、`npx vue-tsc --noEmit`，修掉任何回归。
2. 照 `gemini.rs` 的测试补 `gateway/pipeline_tests/vertex.rs`（现状见上面第 5.1 条），并清理剩余的 dead_code 警告。
3. 用 `npx tauri dev` 真正启动一次，逐页对照 `designs/`，检查 OAuth 打开浏览器、凭据库读写、网关启停。
4. 再尝试 `npx tauri build`（macOS 先不要求签名公证）。
5. 然后再做 kiro、kilocode、kimchi、iflow 的专用授权，以及 github、muse 的令牌兑换。
6. 首次提交前确认 `.gitignore`，不要提交 `node_modules`、`src-tauri/target`、任何凭据。仓库已 `git init`（分支 `main`），**还没有任何提交**。
