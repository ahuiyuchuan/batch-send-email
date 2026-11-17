# v1.0.0 发布前验证

维护者：ahui。日期：2026-11-17。环境：Windows x64、项目隔离 Rust/MSVC 工具链、Node.js、Edge Playwright。界面只按用户指定的 **1920×1080** 检查深浅外观。

## 变更与兼容边界

- `frontend/components/AboutPage.tsx`、`frontend/lib/updates.ts`、`frontend/about.css`：新增关于页、版本状态、项目入口与固定底部更新操作。常驻状态保留跨页结果，手动检查防重，较新的后台结果不被旧响应覆盖。
- `src-tauri/src/updates.rs`、`main.rs`、诊断白名单：启动及每次完成后一小时静默检查；手动入口共用并发门禁。固定公开 GitHub API、SemVer、请求/总超时、响应上限、严格外链、纯文本发布说明，失败不冒充最新版。只打开发布页，不安装更新。
- `frontend/components/AboutPage.tsx`、`frontend/about.css`：恢复既有软件信息、版本更新、项目与反馈及本版功能的纵向卡片布局，仅移除两个许可按钮及未使用弹窗；仅保留项目、发布和反馈三个入口，更新操作固定底部。`vite.config.ts`将完整许可作为`licenses/`纯文本资源随EXE嵌入，无额外按钮、弹窗或运行时许可脚本。
- npm、Cargo、Tauri、窗口、README 和交付文件统一为 `batch-send-email` / `1.0.0`，采用 `GPL-3.0-only`。新增 `scripts/check-release.mjs`、许可生成/校验脚本与发布文档。
- 保留 `com.ahui.qq-group-exporter`、既有浏览器存储键、凭据绑定及数据目录，SQLite schema 不变。没有搬迁或清空用户数据。QQ登录继续只在当前进程有效；SMTP密文继续绑定原Windows用户/设备。
- 独立核对锁定的 single-instance 2.5.5 与 Tauri 2.12.5 源码/功能树：未启用单实例semver隔离，Windows锁和WebView默认目录均由保留的identifier派生，改产品名和EXE名不会分裂这两个边界。
- 原有多群、邮件、模板、历史、手动排除与偏好记忆继续保留。本项目使用Rust/TypeScript，Java手册中的Java/MyBatis专用规则不适用；注释与格式遵循本项目实际工具。

## 实际执行及结果

| 命令或检查                                                                                              | 结果                                                                                                                      |
| ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `npm run format:check`、`npm run typecheck`                                                             | 通过。                                                                                                                    |
| `npm run test:sort`、`npm run test:preferences`、`npm run test:groups`                                  | 10 + 5 + 2 = 17项通过。                                                                                                   |
| `UI_CHECK_ONLY=detail-polish npm run test:ui`                                                           | 完整1920×1080隔离回归通过，覆盖五个页面及现有业务弹窗、深浅外观、空/长内容、防重、失败重试、取消、跨页草稿和固定操作区。  |
| `cargo fmt --all -- --check`                                                                            | 通过。                                                                                                                    |
| `cargo test --workspace --locked`                                                                       | 121项宿主 + 166项核心 + 1项运行日志集成 = 288项通过；1项显式公网发布测试按设计忽略。                                      |
| `cargo clippy --workspace --all-targets --locked -- -D warnings`                                        | 通过，零警告。                                                                                                            |
| `node scripts/check-release.mjs`                                                                        | npm/锁文件/Cargo/Tauri版本、名称、GPL及旧identifier一致。                                                                 |
| `node scripts/generate-notices.mjs --check`                                                             | 9个npm、373个Cargo依赖；许可合订本与锁定本地依赖一致。                                                                    |
| `npx prettier --tab-width 2 --check scripts/generate-notices.mjs scripts/check-release.mjs`             | 通过。                                                                                                                    |
| `pwsh -NoProfile -File scripts/build.ps1 -OutputDirectory dist/v1.0.0-final`（`UI_CHECK_ONLY=detail-polish`） | 上述检查及Windows x64单文件发布构建通过。                                                                                 |
| `UI_CHECK_ONLY=about` 回归与 `npm run typecheck`                                                        | 通过；确认两个许可按钮移除、三个项目入口、既有纵向卡片、六项功能列表及固定更新操作，启动默认版本读取package配置。 |
| `pwsh -NoProfile -File scripts/build.ps1 -SkipTests -OutputDirectory dist/v1.0.0-final`                       | 在同版本完整回归后，针对恢复原布局及移除两按钮完成专项回归并重新打包；元数据/许可仍强制核对。                                                       |
| `UI_CHECK_ONLY=production-about npm run test:ui`                                                        | 编译资源覆盖三个项目入口、原有纵向卡片布局、当前/无发布/失败/重试、新版、长说明、事件竞态、防重、草稿和深浅色。                   |
| `python scripts/check-exe-icon.py --exe dist/v1.0.0-final/batch-send-email.exe`                               | 七种内嵌图标与源ICO逐字节一致；PE头确认x64/PE32+，产品及文件版本均1.0.0。                                                 |

全量UI还覆盖多群跨搜索选择/全选、去重及来源、10005人完整导出、过滤恢复、多账号1/2/3/5/8/10人均分、历史账号归属、未知投递人工核实、独立冷却/等待跳过、动态预计结束时间、模板交换和扫码替换等隔离契约。没有在上述流程中发起真实QQ或SMTP请求。

源码、配置和文档重新读取确认UTF-8；与修改前快照比较，BOM与既有LF/CRLF均未意外改变。新增公开文档、注释、配置、使用说明、README截图、GPL全文与第三方许可同步。发布前检查暂存差异、文件清单、本地链接与敏感文件排除；构建缓存、工具链、运行数据库、凭据、完整日志和旧测试包不入库。

全仓 `git diff --cached --check` 提示原始许可文本的空白、历史Markdown换行标记及文件末尾空行；保留原始许可和既有文档格式。业务源码与脚本另行按行末空白/制表符检查通过，没有把第三方条款的空白格式作为代码自动修复。

## 产物与回退

最终交付为 `Batch-Send-Email-v1.0.0-Windows-x64.exe`，**18,473,472字节**。

```text
SHA-256 0732C2A0A2E070ADE92B7425E5B3282E63C18F044319DC73D855D5A905EE0AD9
```

正式Release仅上传该EXE，版本Tag提供对应源码；SHA-256同步列入Release正文。原有测试包仍保存在本地忽略目录，未覆盖或删除。回退前完全退出程序；若需回到升级前完整业务状态，恢复原先备份的整个应用数据目录，不删除数据库或手动降低schema。

## 限制与发布后闭环

没有执行真实QQ成员读取、SMTP投递、收件箱送达、长期运行或无开发环境机器原生GUI测试；Playwright是编译页面和模拟IPC验证，不能代替这些联调。没有读取用户真实数据库或凭据，也没有启动会访问这些数据的原生EXE。

Vite仍报告主资源超过500kB的体积提示；许可文本作为独立纯文本资源压缩内嵌，不生成运行时脚本、不随首屏加载。该提示不是构建错误，暂不进行与本次业务无关的拆包重构。

正式发布后显式运行 `cargo test -p batch-send-email live_public_release --locked -- --ignored`，使用生产更新客户端读取真实公开Release；常规测试保持离线。另核对Tag提交、公开最新版本、唯一资产及下载文件SHA-256，结果保存在本机发布凭据记录并补入Release正文。真实公开API检查已执行通过，返回当前正式版1.0.0；最终EXE重新构建后，生产关于页回归、许可原文完整性、图标及文档链接检查亦通过。
