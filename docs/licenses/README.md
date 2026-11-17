# 第三方许可证材料

维护者：ahui

本目录保留实际依赖及其包内附带组件的版权、许可和声明，供构建许可合订本及源码查阅使用。`batch-send-email` 自有代码采用 GPL-3.0-only；该许可不替换第三方组件各自的许可，也不改变 Microsoft WebView2 Runtime 的独立分发条款。

## 生成与核对

先使用 `npm ci` 和项目 Rust 工具链准备锁文件对应的本地依赖，再从仓库根目录执行：

```powershell
node scripts/generate-notices.mjs
node scripts/generate-notices.mjs --check
```

生成器不联网、不读取应用数据库或账号配置，产物为 [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt)。`--check` 只比较现有文件；缺少依赖、安装版本与锁文件不符、缺少许可原文或产物过期时返回失败。失败不会改写已有合订本。

收录范围为 `package-lock.json` 中的生产前端依赖，以及 `Cargo.lock` 在 `x86_64-pc-windows-msvc` 下的正常和构建依赖。开发工具、仅测试依赖与本项目工作区 crate 不在第三方运行依赖清单中。包目录中附带的 `LICENSE`、`LICENCE`、`COPYING`、`NOTICE`、`UNLICENSE` 文件递归收录，符号链接不跟随；许可内容仅统一换行为 LF，以 UTF-8 无 BOM 写入，文件来源只记录包内或仓库相对路径。

修改依赖、锁文件、项目名称或版本后，应重新生成并核对差异。正式 Release 只提供单个 EXE；Vite 将根目录 `LICENSE`、`THIRD_PARTY_NOTICES.md` 和许可合订本生成到编译资源的 `licenses/` 目录，作为三个文本资源随前端资源嵌入 EXE。许可原文、来源及相应源码保留在仓库，并通过同一 Release 的版本 Tag 提供。

## 上游声明缺项的核对副本

以下固定版本的 crates.io 包未包含独立许可文件。生成器只对表内精确版本使用核对副本，不对未知版本猜测许可；依赖升级时需重新核验上游声明。

| 本地副本                                               | 适用依赖版本                                                                  | 上游来源与核对依据                                                                                                                                                    |
| ------------------------------------------------------ | ----------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [alloc-stdlib-LICENSE](alloc-stdlib-LICENSE)           | `alloc-stdlib 0.3.0`                                                          | [Dropbox rust-alloc-no-stdlib/LICENSE](https://github.com/dropbox/rust-alloc-no-stdlib/blob/master/LICENSE)，BSD-3-Clause，版权归 2016 Dropbox。                      |
| [selectors-LICENSE-MPL-2.0](selectors-LICENSE-MPL-2.0) | `selectors 0.38.0`                                                            | [Servo stylo](https://github.com/servo/stylo) 的包元数据与源码头声明 MPL-2.0；保存标准 MPL 2.0 全文，许可正文亦见 [Mozilla](https://www.mozilla.org/en-US/MPL/2.0/)。 |
| [tauri-LICENSE-MIT](tauri-LICENSE-MIT)                 | `tauri-plugin 2.7.2`                                                          | [Tauri/LICENSE-MIT](https://github.com/tauri-apps/tauri/blob/dev/LICENSE-MIT)；包声明为 Apache-2.0 OR MIT，此处保留 MIT 选项原文。                                    |
| [webview2-LICENSE](webview2-LICENSE)                   | `webview2-com 0.39.1`、`webview2-com-macros 0.8.1`、`webview2-com-sys 0.39.1` | [webview2-rs/LICENSE](https://github.com/wravery/webview2-rs/blob/main/LICENSE)，MIT，版权归 2021 Bill Avery。                                                        |

这些副本已结合本项目锁定版本的元数据、源码声明及表中上游来源核对，没有修改许可条款。完整组件信息以生成合订本和两个锁文件为准，组件说明见根目录 [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md)。
