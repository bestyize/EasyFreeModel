# FreeApiBox · 免费 API 盒子

把自己的大模型 API 连接集中到一个本地入口，方便接入 Codex、Claude Code 和其他客户端。FreeApiBox 是一款免费使用的桌面工具：管理提供商与账户、创建本地 API 密钥、查看调用记录，并通过兼容接口向客户端提供模型。

> **“免费”指软件本身免费。** 模型是否提供免费额度、额度大小和超额费用由各提供商决定。使用前请查看上游平台的条款与账单。

项目仍在开发中。提供商目录里的“待接入”表示该提供商尚不能正常转发请求；请优先选择页面上标记为可用的提供商，或添加兼容的自定义提供商。

## 它能做什么

- **集中管理连接**：在“提供商”页添加账户、API Key 和模型；支持自定义 OpenAI 兼容或 Anthropic 兼容端点。提供商页面会展示适配器是否就绪，不能仅凭目录中出现某个名字判断其可用。
- **统一本地入口**：本地网关提供 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages、Gemini 和 Ollama 风格的入口；具体请求能否成功还取决于上游提供商、模型与对应适配器。
- **接入常用客户端**：在“API 密钥”页选好密钥和模型后，可预览并写入 Codex、Claude Code 的本机配置（写入前会备份原文件）；CC Switch 提供导入入口。
- **查看运行情况**：管理本地访问密钥，查看请求、Token 用量和配额信息；配额数据是否能实时获取取决于提供商支持情况。

## 下载与安装

在仓库的 **Releases** 页面选择与你的设备对应的安装包：

| 平台 | 安装包 | 说明 |
| --- | --- | --- |
| Windows x64 | `.exe` | NSIS 安装程序 |
| macOS Apple Silicon（ARM64） | `.dmg` | 适用于 M 系列芯片 |

安装包由手动触发的 [BuildPackage 工作流](.github/workflows/build-package.yml) 构建并发布；如果 Releases 页面暂时没有安装包，可以按下文从源码运行。目前不提供 Intel Mac 或 Linux 安装包。安装包未使用正式的开发者签名，操作系统可能显示安全提示；请核对下载来源与 Release 中的 `SHA256SUMS.txt`。

## 五分钟接入

1. 启动 FreeApiBox，进入 **提供商**，选择标记为可用的提供商，添加账户或 API Key，并确认目标模型已启用。也可以添加自定义 OpenAI／Anthropic 兼容端点。
2. 进入 **API 密钥**，创建本地访问密钥。在“连接参数”中选择该密钥和想用的模型。这里的密钥用于访问 FreeApiBox 网关，与提供商的上游 API Key 是两种不同的凭据。
3. 进入 **API 接入**，启动本地网关，复制页面显示的地址和请求示例。默认监听 `127.0.0.1:20128`，只能由本机访问；如果你修改过端口，以应用中显示的地址为准。
4. 需要接入 **Codex** 或 **Claude Code** 时，回到 **API 密钥 → 一键接入**，预览配置并确认写入；使用 **CC Switch** 时选择对应的导入入口。写入客户端配置后，按客户端要求重新启动或打开新会话。

也可以先用下面的命令检查网关。把 `YOUR_LOCAL_KEY` 换成第 2 步创建的本地访问密钥：

```bash
curl http://127.0.0.1:20128/health

curl http://127.0.0.1:20128/v1/models \
  -H 'Authorization: Bearer YOUR_LOCAL_KEY'
```

`/health` 不需要密钥；`/v1/models` 和模型调用需要密钥。如果模型列表为空，请检查提供商适配器、连接状态和模型启用状态。一个 OpenAI Chat 请求示例：

```bash
curl http://127.0.0.1:20128/v1/chat/completions \
  -H 'Authorization: Bearer YOUR_LOCAL_KEY' \
  -H 'Content-Type: application/json' \
  -d '{"model":"YOUR_MODEL","messages":[{"role":"user","content":"你好"}]}'
```

将 `YOUR_MODEL` 替换为“连接参数”里显示的模型名。“API 接入”页还提供 OpenAI Responses、Anthropic Messages、Gemini 的 curl、Python 或 Node.js 示例（Gemini 目前提供 curl 示例）。

### 常用接口地址

默认地址如下；若修改了监听地址或端口，请以应用的“API 接入”页为准。

| 协议 | 请求路径 | 客户端 Base URL |
| --- | --- | --- |
| OpenAI Chat Completions | `/v1/chat/completions` | `http://127.0.0.1:20128/v1` |
| OpenAI Responses | `/v1/responses` | `http://127.0.0.1:20128/v1` |
| Anthropic Messages | `/v1/messages` | `http://127.0.0.1:20128` |
| Gemini | `/v1beta/models/{model}:generateContent` | `http://127.0.0.1:20128` |
| Ollama | `/api/chat` | 以客户端要求填写 |
| 模型列表 | `/v1/models` | 需要本地 API 密钥 |

## 从源码运行

需要 **Node.js 22**、npm 和 **Rust 1.98 或更新版本**，以及 Tauri 2 在当前系统要求的构建依赖。克隆仓库后，在项目根目录运行：

```bash
npm ci
npm run tauri -- dev
```

常用开发命令：

```bash
npm run dev        # 浏览器界面预览，使用 mock 数据；不启动真实桌面网关
npm run typecheck  # 前端类型检查
npm test           # 前端测试
npm run build      # 前端生产构建
```

桌面应用由 Vue 3、TypeScript、Tauri 2 和 Rust 构成。需要自己打包时，可在 GitHub Actions 中手动运行 **BuildPackage**；发布前请检查两端版本号及安装包内容。

## 使用边界与安全

- 默认只监听本机回环地址。若在“设置”中改为 `0.0.0.0` 或其他非回环地址，同一网络中的设备可能访问网关；请妥善保管本地访问密钥，并自行处理网络隔离。
- 上游凭据保存在本机应用数据目录的凭据文件中；目前该文件**没有额外加密**，类 Unix 系统将其权限设置为仅当前用户可读写。请保护你的系统账户和备份，不要分享包含凭据的应用数据。
- 请求记录和配额视图用于辅助查看使用情况。上游计费、免费额度与实时余额请以提供商后台为准；部分配额暂时无法自动获取。
- 自动更新尚未接入；图像、语音、视频和联网搜索页面目前不在主导航中，不能视为已完成的功能。

## 许可与项目资料

项目源码可公开查看，但仓库目前**没有 `LICENSE` 文件**，尚未声明开源许可证。若你计划分发、修改或在其他项目中复用代码，请等待维护者明确授权条款。

- [产品与 UI 设计规范](PRODUCT_DESIGN.md)
- [设计稿](designs/)
- [第三方声明](THIRD_PARTY_NOTICES.txt)
