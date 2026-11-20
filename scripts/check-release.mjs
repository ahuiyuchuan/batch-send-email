/**
 * 构建前核对发布元数据，防止界面、更新比较、Tag与EXE使用不同版本。
 * 只读取受版本控制的配置；固定旧标识用于兼容既有数据库、WebView和单实例边界。
 * @author ahui
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
const pkg = JSON.parse(read("package.json"));
const lock = JSON.parse(read("package-lock.json"));
const tauri = JSON.parse(read("src-tauri/tauri.conf.json"));
assert.match(pkg.version, /^\d+\.\d+\.\d+$/);
assert.equal(pkg.name, "batch-send-email");
assert.equal(pkg.license, "GPL-3.0-only");
assert.equal(lock.name, pkg.name);
assert.equal(lock.version, pkg.version);
assert.equal(lock.packages[""].version, pkg.version);
assert.equal(tauri.productName, pkg.name);
assert.equal(tauri.version, pkg.version);
assert.equal(tauri.identifier, "com.ahui.qq-group-exporter");
for (const path of ["src-tauri/Cargo.toml", "crates/member-core/Cargo.toml"]) {
  const packageSection = read(path).split(/\n\[/)[0];
  assert.equal(
    packageSection.match(/^version = "([^"]+)"/m)?.[1],
    pkg.version,
    path,
  );
  assert.equal(
    packageSection.match(/^license = "([^"]+)"/m)?.[1],
    pkg.license,
    path,
  );
}
assert.equal(
  read("src-tauri/Cargo.toml").match(/^name = "([^"]+)"/m)?.[1],
  pkg.name,
);
console.log(
  `${pkg.name} v${pkg.version}: metadata consistent (${fileURLToPath(root)})`,
);
