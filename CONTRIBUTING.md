# 开发与贡献

作者：ahui。提交前使用虚构账号和名单，不提交授权码、Cookie、真实邮件、数据库或本机凭据。

## 环境与检查

- Windows 10 / 11 x64、Microsoft Edge WebView2 Runtime。
- Node.js 22.12 或更新的受支持版本、npm。
- Rust stable MSVC 工具链、MSVC C++ Build Tools 与 Windows SDK；首次构建需下载锁定依赖。
- Python 3 与 Pillow 仅用于重建／核对图标；普通 EXE 运行不依赖这些开发工具。

```powershell
npm ci
npm run dev
npm run tauri -- dev
npm run format:check
npm run typecheck
npm run test:sort
npm run test:preferences
npm run test:groups
node scripts/generate-notices.mjs --check

# 本项目界面验收固定1920×1080，含深浅色和隔离业务流程。
$env:UI_CHECK_ONLY = 'detail-polish'
npm run test:ui
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

# 完整检查和正式EXE；脚本也支持项目.tools中的隔离工具链。
pwsh -NoProfile -File scripts/build.ps1 -OutputDirectory dist/v1.0.0
```

请勿把没有执行的检查写成通过。界面回归使用合成 IPC，后端网络测试只使用本机模拟；二者不能证明真实 QQ 权限、服务端完整性、SMTP 接受、额度或收件箱送达。涉及真实账号的验证须明确授权并保护凭据。实际结果记录在 [VALIDATION](VALIDATION.md)。

## 代码与数据边界

源码使用 UTF-8 无 BOM。Rust／TypeScript／CSS 沿用 LF 和项目格式化规则，现有 PowerShell 使用 CRLF；读取中文明确指定 UTF-8。公开 API、业务约束、并发、异常、缓存与兼容逻辑须有准确注释；新文件作者为 `ahui`，不伪造旧文件作者或日期。

功能入口与目录说明见 [DEVELOPMENT](docs/DEVELOPMENT.md)。不为小改动引入无关重构；修改默认值、SQLite、IPC、配置或公共结构时同步用法、迁移／回滚、示例及有意义的回归测试。生成文件和锁文件通过相应工具更新，不直接手改依赖解析结果。

项目为 Rust / TypeScript，Java 语法、Javadoc 和 MyBatis 专属规范不适用；通用命名、注释、输入校验、异常处理和安全要求继续采用。

必须保留的行为：

- QQ 获取会话和目标归属一致；逐群核验后合并，失败／取消不提交部分结果，好友列表不跨账号复用。
- 单群／组合快照、导出、恢复及邮件范围按固定版本处理；补充邮箱不能绕过已过滤／未选成员限制。
- 发信预检固定实际去重名单与随机分配；所有账号共享历史与手动排除，待核实投递不视为可重发。
- 同账号冷却跨任务保留，切换等待独立计算；跳过只影响一次等待，不清空冷却记录。
- 账号、主题、正文、资源和原始响应不进入诊断日志；更新检查与业务锁、QQ／SMTP凭据和数据库隔离。
- 更名不改变 `com.ahui.qq-group-exporter` 数据兼容键，不静默创建空数据库代替旧数据。

## 提交、Tag 与发布

提交采用 `feat:` / `fix:` / `docs:` 等明确前缀和中文主题。正文围绕最终行为按职责分节，写清交互、异常／边界、数据兼容和实际验证，避免重复叙述过程。不要在提交消息中包含真实账号、路径、邮件或日志原文。

版本使用语义版本；Tag 为带说明的 `v<版本>`，首行 `batch-send-email v<版本>`，随后概括功能、交互、兼容与验证限制。GitHub Release 标题采用相同形式，正文来自 `docs/release-notes/v<版本>-zh.md`；用户可见变化同步写入 `CHANGELOG.md`，npm／Cargo／Tauri版本及下载链接保持一致。

正式发布只上传单个 `Batch-Send-Email-v<版本>-Windows-x64.exe`，不上传 ZIP、数据库、文档包或独立校验文件；SHA-256 放在发布正文，本地校验文件可保留。GitHub 自动生成的 Source code 归档不是手工上传资产。发布前核对版本、PE x64、构建新鲜度、图标、复制前后摘要和源码对应关系；先完成检查与审阅，再推送提交／Tag 和创建 Release。

依赖、锁文件、项目名称或版本变更后，先运行 `node scripts/generate-notices.mjs`，再用 `--check` 核对；生成失败时不以缺项声明发布。Vite 将项目协议、第三方说明和运行依赖许可合订本生成到编译资源的 `licenses/` 目录，并随前端资源嵌入 EXE；许可原文、核对来源及对应源码保留在仓库和同版本 Tag 中。维护范围见 [许可证材料](docs/licenses/README.md)。

截图只使用虚构数据；仓库保留 README 引用图片，完整日志、QA截图、工具链、缓存与发布包保留在忽略目录。GPL-3.0-only适用于本项目代码，依赖保留各自许可证；新增依赖同步 [第三方声明](THIRD_PARTY_NOTICES.md)。
