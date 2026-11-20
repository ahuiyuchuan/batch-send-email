# v1.0.1 发布验证

维护者：ahui。日期：2026-11-20。环境：Windows x64、项目隔离 Rust/MSVC 工具链、Node.js、Edge Playwright。界面仅按用户指定的 **1920×1080** 检查深浅外观。v1.0.0 历史记录保留在对应 Tag。

## 修复与影响范围

- Issue #1 的直接原因是前端正文清理规则未保留链接标签，模板、预览和发送共用此规则；后端纯文本替代内容也未保留链接目标。
- `frontend/lib/mail-links.ts`、`MailLinkDialog.tsx`、`MailComposer.tsx`：新增插入、编辑、移除链接及正文内 Ctrl+K。取消保留选区，操作可撤销，继续输入不延长链接。弹窗复用固定标题及底部操作区，错误紧邻字段，中文输入法确认候选不误提交。跨段落、图片或多链接混选不破坏正文。
- 正文及模板只保留通过校验的链接。编辑区阻止跳转；预览显示可复制的完整目标且不导航，保留 iframe sandbox、CSP 和资源限制。
- `crates/member-core/src/mail.rs` 独立校验安全地址，HTML 邮件保留链接，纯文本替代内容补充目标；原有 CID 图片、资源登记、脚本及外部图片限制保持生效。
- 新增 `frontend/tests/mail-link-check.mjs` 并接入全量及编译资源回归。同步业务注释、使用手册、CHANGELOG、版本及生成许可；README 仅同步版本、下载入口和链接能力，不重排布局。
- 无新依赖、IPC 或数据库迁移：SQLite schema 5、模板格式 1、`com.ahui.qq-group-exporter`、数据目录、凭据绑定、账号、冷却和排除规则不变。未读取真实数据库或凭据。项目采用 Rust/TypeScript，Java/MyBatis 专属规则不适用，通用格式、注释与校验规则继续遵守。

## 实际执行及结果

| 命令或检查 | 结果 |
| --- | --- |
| `npm run format:check`、`npm run typecheck` | 通过。 |
| `npm run test:sort`、`npm run test:preferences`、`npm run test:groups` | 10 + 5 + 2 = 17 项通过。 |
| `UI_CHECK_ONLY=mail-links npm run test:ui` | 链接专项通过，覆盖插入、编辑、移除、撤销、格式、焦点选区、非法输入、IME、模板交换、预览与合成发送内容。 |
| `UI_CHECK_ONLY=detail-polish npm run test:ui` | 1920×1080 全量隔离回归通过，覆盖深浅色、业务页面、弹窗、固定操作区与原有业务流程。 |
| `cargo fmt --all -- --check` | 通过。 |
| `cargo test --workspace --locked` | 121 项宿主 + 169 项核心 + 1 项日志集成 = **291 项通过**；1 项显式公网测试按设计忽略。 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，零警告。 |
| `node scripts/check-release.mjs` | npm/锁文件/Cargo/Tauri 名称、版本、GPL 与兼容 identifier 一致。 |
| `node scripts/generate-notices.mjs --check` | 9 个 npm、373 个 Cargo 依赖许可与锁定依赖一致，完整许可随编译资源嵌入 EXE。 |
| `pwsh -NoProfile -File scripts/build.ps1 -OutputDirectory dist/v1.0.1`（`UI_CHECK_ONLY=detail-polish`） | 上述检查及 Windows x64 单文件发布构建全部通过。 |
| `UI_CHECK_ONLY=production-mail-links npm run test:ui` | 正式编译资源的链接交互、模板与合成发送流程通过。 |
| `UI_CHECK_ONLY=production-about npm run test:ui` | 正式编译资源的更新状态、失败重试、防重、事件竞态、长说明、入口和深浅色通过。 |
| `python scripts/check-exe-icon.py --exe dist/v1.0.1/batch-send-email.exe` | 七种内嵌图标与源 ICO 逐字节一致；另核对 PE 为 x64/PE32+，文件/产品版本均为 1.0.1。 |

后台链接测试覆盖安全净化与 CID 共存、纯文本目标保留、最终内存 MIME 的 HTML/plain 双部件，不建立 SMTP 连接。独立复核另执行 13 个 URL 边界样例并检查 Enter、取消、移除、IME、选区及 dialog inert 解除时序，未发现发布阻断问题。

全量 UI 还覆盖多群搜索和全选、10005 人完整导出、去重与来源、过滤恢复、多账号均分、历史发件账号、未知投递核实、冷却与跳过等待、动态预计结束时间、模板交换失败与重试、扫码切换、草稿和资源保留。人工核对虚构数据截图中的链接弹窗、预览、发送选项、账号弹窗的对齐、间距和固定操作区。

## 产物与兼容

正式发布仅上传 `Batch-Send-Email-v1.0.1-Windows-x64.exe`，**18,490,880 字节**；复制前后与构建源 EXE 的 SHA-256 一致。

```text
SHA-256 404E1776AD9B5B9DAB51296467B2AD1085B91B849A3E5E3B130680B0EBAD9F69
```

源码、配置与文档使用 UTF-8 无 BOM，业务源码沿用 LF；不改变既有 PowerShell 换行。完整日志、截图、工具链、数据库、凭据及测试产物留在忽略目录。提交前核对差异、文件范围、许可与本地文档链接；Git 原有 autocrlf 设置不作为批量重写文件的理由。

升级前完全退出旧版并备份整个应用数据目录。回退 v1.0.0 不需降低 schema，但旧版编辑含链接内容会清除链接，已丢失的目标不能自动恢复；需完整恢复时退出应用并恢复原备份。保留 v1.0.0 Tag 与 Release。

## 验证限制与发布闭环

未执行真实 QQ 读取、SMTP 投递、收件箱送达、长期运行或无开发环境机器原生 GUI 联调。Playwright 使用隔离 IPC；内存 MIME 与本机模拟不能证明服务商接受或送达。未启动会访问真实数据的原生 EXE。

Vite 仍有主资源超过 500 kB 的体积提示，构建成功；本次不扩展为无关拆包重构。

发布后另以生产更新客户端显式执行 `cargo test -p batch-send-email live_public_release --locked -- --ignored`，核对公开最新版本、Tag 提交、唯一 EXE 及公网下载 SHA-256。结果保存到本机忽略的发布核验记录；常规测试继续保持该公网检查显式忽略。
