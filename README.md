# FreeApiBox · 免费 API 盒子

免费使用的桌面 AI 模型 API 连接工具。将多个模型供应商、账户和 API 协议集中到一个本地入口，管理 API 密钥、查看用量与配额，并接入 Codex、Claude Code 等客户端。

软件本身免费；可用模型、免费额度及调用费用由各供应商决定。FreeApiBox 不提供免费调用所有模型的承诺。

## 下载与开发

Windows x64 和 macOS Apple Silicon 的安装包通过手动触发的 [BuildPackage](https://github.com/bestyize/EasyFreeModel/actions/workflows/build-package.yml) 工作流构建，并发布在 [Releases](https://github.com/bestyize/EasyFreeModel/releases)。

本地开发需要 Node.js 22 与 Rust（1.98 或更新版本）：

```bash
npm ci
npm run tauri -- dev
```

## 项目资料

- [产品与 UI 设计规范](PRODUCT_DESIGN.md)
- [设计稿](designs/)
- [第三方声明](THIRD_PARTY_NOTICES.txt)
