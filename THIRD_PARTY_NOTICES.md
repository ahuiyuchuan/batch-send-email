# 第三方组件许可声明

维护者：ahui

`batch-send-email` 为 QQ 群成员整理与多账号邮件发送工具。本项目自有代码采用 GPL-3.0-only，完整条款见 [LICENSE](LICENSE)；下列第三方组件保留各自版权和许可证，不因本项目协议而被重新授权。

发布随附的完整依赖许可证合订本见 [THIRD_PARTY_LICENSES.txt](docs/licenses/THIRD_PARTY_LICENSES.txt)，生成范围、来源和缺项核对说明见 [许可证维护说明](docs/licenses/README.md)。项目协议、本说明和许可合订本作为 `licenses/` 文本资源随 EXE 嵌入，许可原文及对应源码保留在仓库和同版本 Tag 中。

## 桌面与界面依赖

Tauri、React、React DOM、Lucide、Rust crates 及前端构建工具均保留其原有版权和许可。准确版本以 `Cargo.lock` 与 `package-lock.json` 为准；相应上游许可证继续适用。本项目没有修改、重新授权或声称拥有这些组件。

- [Tauri](https://github.com/tauri-apps/tauri)：桌面窗口与 WebView2 集成。
- [React](https://github.com/facebook/react)：界面组件与状态更新。
- [Sonner](https://github.com/emilkowalski/sonner)：2.0.8，MIT；操作反馈浮层，版权和许可全文见下文。
- [pinyin-pro](https://github.com/zh-lx/pinyin-pro)：3.29.5，MIT；中英文共用拼音首字母排序，版权与许可全文见下文。
- [Lucide](https://github.com/lucide-icons/lucide)：界面图标。
- [rust_xlsxwriter](https://github.com/jmcnamara/rust_xlsxwriter)：生成 Excel 工作簿。
- [Reqwest](https://github.com/seanmonstar/reqwest)、[Tokio](https://github.com/tokio-rs/tokio)、[Serde](https://github.com/serde-rs/serde)、[Rust CSV](https://github.com/BurntSushi/rust-csv)：网络、异步执行、序列化和 CSV。
- [Lettre](https://github.com/lettre/lettre)、[Rusqlite](https://github.com/rusqlite/rusqlite)、[Ammonia](https://github.com/rust-ammonia/ammonia)：SMTP、SQLite 和 HTML 清理。

邮件内容模板复用已有 Rusqlite 事务和 Ammonia HTML 清理，资源以本机 SQLite BLOB 保存；本功能未新增运行依赖或常驻服务。

- [cookie_store](https://github.com/pfernie/cookie_store)、[Zeroize](https://github.com/RustCrypto/utils/tree/master/zeroize)：在当前进程内保留 Cookie 原始作用域和服务端到期信息，并清理敏感临时缓冲区；QQ 登录不持久化，Windows DPAPI 用于保护本机 SMTP 授权码。
- [Image](https://github.com/image-rs/image)、[Base64](https://github.com/marshallpierce/rust-base64)：PNG/JPEG/GIF/WebP 图片格式与尺寸检查、本机图片预览及 MIME 编码。未开启其他图片格式或默认格式集合。

Microsoft Edge WebView2 Runtime 是独立的微软运行时，适用微软的许可与分发条款。本项目使用系统安装的运行时，没有将完整浏览器内核打入便携 EXE。

[Pillow](https://github.com/python-pillow/Pillow) 仅用于开发脚本从保留的应用标识原图生成 PNG/ICO；适用对应版本的上游许可，不是 EXE 的运行依赖。

## QQ 网页接口行为参考

好友读取行为参考 QQ 网页部署 `7904f18b` 的公开静态脚本：[请求封装](https://cdn-go.cn/qq-qun-web/7904f18b/assets/index.85819934.js) 与 [好友选择页](https://cdn-go.cn/qq-qun-web/7904f18b/assets/index.2d8ec08f.js)。本项目仅据其调用方式和分组响应消费方式编写独立 Rust 实现，没有新增源码依赖、运行依赖或常驻服务；这些链接用于维护核对，实际接口仍由 QQ 控制。

## Sonner 2.0.8 许可

以下内容来自依赖包的 `sonner/LICENSE.md`，界面集成未修改其许可。

```text
MIT License

Copyright (c) 2023 Emil Kowalski

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## pinyin-pro 3.29.5 许可

以下内容来自依赖包的 `pinyin-pro/LICENSE`，使用随包词库的单字默认音；未修改上游源码。

```text
MIT License

Copyright (c) 2022-present zh-lx

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
