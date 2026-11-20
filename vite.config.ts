/** Frontend build configuration. All runtime resources remain inside the desktop EXE. @author ahui */
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";

export default defineConfig({
  plugins: [
    react(),
    {
      name: "embedded-license-texts",
      /** 随发布资源保留原许可全文；不再为了展示协议增加页面按钮、弹窗或运行时JS。 */
      generateBundle() {
        for (const [name, source] of [
          ["LICENSE.txt", "LICENSE"],
          ["THIRD_PARTY_NOTICES.txt", "THIRD_PARTY_NOTICES.md"],
          [
            "THIRD_PARTY_LICENSES.txt",
            "docs/licenses/THIRD_PARTY_LICENSES.txt",
          ],
        ]) {
          this.emitFile({
            type: "asset",
            fileName: `licenses/${name}`,
            source: readFileSync(new URL(source, import.meta.url)),
          });
        }
      },
    },
  ],
  clearScreen: false,
  // Rust/Tauri generates HTML under target; it must not become a second Vite
  // entry or reload the renderer while the independent desktop build is running.
  optimizeDeps: {
    entries: ["index.html"],
  },
  server: {
    watch: {
      ignored: [
        "**/target/**",
        "**/.tools/**",
        "**/artifacts/**",
        "**/web-dist/**",
        // 发布副本不参与热更新，避免 Windows 写入/扫描 EXE 时开发监听报文件占用。
        "**/dist/**",
      ],
    },
  },
  build: {
    outDir: "web-dist",
    emptyOutDir: true,
    sourcemap: false,
    target: "es2022",
  },
});
