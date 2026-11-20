/**
 * 从锁定的本地 npm/Cargo 依赖生成发布用许可证合订本，不联网、不读取应用数据。
 * 先按锁文件准备依赖，再运行 node scripts/generate-notices.mjs；--check 只核对产物。
 * Windows 依赖图只遍历正常和构建边，不包含仅测试依赖；包内嵌的 LICENSE/NOTICE 也一并收录。
 * 缺少安装包、版本不符或许可证缺项时整次失败，保留已有产物；不以声明名称伪造许可原文。
 * @author ahui
 */
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputPath = join(root, "docs/licenses/THIRD_PARTY_LICENSES.txt");
const check = process.argv.includes("--check");
const readText = (path) => readFileSync(path, "utf8").replace(/\r\n/g, "\n");
const project = JSON.parse(readText(join(root, "package.json")));
const pieces = [
  `${project.name} v${project.version} — Third-party licenses\n整理：ahui\n\n` +
    "生成来源：scripts/generate-notices.mjs、package-lock.json、Cargo.lock 及本机锁定依赖包。\n" +
    "范围：生产前端的全部已安装传递依赖，以及 Windows x64 Cargo 正常／构建依赖。\n" +
    "第三方版权与许可证保留；仅统一换行为 LF，未改写许可条款。开发工具及仅测试依赖不在此范围。\n" +
    "声明缺项的核对副本及来源见 docs/licenses/README.md。\n\n",
];

/**
 * 固定名字过滤仅收录声明文件；跳过符号链接，避免从包内越界读取用户或系统文件。
 * @param {string} directory 已安装依赖包的绝对目录；必须存在且可读。
 * @returns {string[]} 按包内目录顺序排列的声明文件绝对路径；无声明时为空数组。
 */
function licenseFiles(directory) {
  const files = [];
  const visit = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true }).sort(
      (a, b) => a.name.localeCompare(b.name, "en"),
    )) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) visit(path);
      else if (
        entry.isFile() &&
        /^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)([-._].*)?$/i.test(
          entry.name,
        )
      )
        files.push(path);
    }
  };
  visit(directory);
  return files;
}

/**
 * 追加原始声明及包内相对位置，不包含构建机绝对路径；外部副本使用仓库相对路径。
 * @param {string} kind 依赖来源分类，仅由生成流程传入 npm 或 crate。
 * @param {object} pkg 本机安装包或 Cargo metadata 的已锁定包信息。
 * @param {string} directory 依赖包绝对目录，用于生成可移植的相对文件标签。
 * @param {string[]} files 已验证来源的声明文件；空数组会终止生成，避免静默漏项。
 * @throws {Error} 无声明文件或读取失败时抛出；此时尚未改写最终合订本。
 */
function append(kind, pkg, directory, files) {
  if (!files.length)
    throw new Error(`缺少 ${kind} 许可证：${pkg.name} ${pkg.version}`);
  pieces.push(
    `\n${"=".repeat(78)}\n${kind}: ${pkg.name} ${pkg.version}\n` +
      `License expression: ${typeof pkg.license === "string" ? pkg.license : JSON.stringify(pkg.license)}\n` +
      `Source: ${typeof pkg.repository === "string" ? pkg.repository : (pkg.repository?.url ?? "package registry")}\n`,
  );
  for (const path of files) {
    const label =
      path.startsWith(directory + "/") || path.startsWith(directory + "\\")
        ? relative(directory, path)
        : relative(root, path);
    pieces.push(
      `\n--- ${label.replaceAll("\\", "/")} ---\n\n${readText(path)}\n`,
    );
  }
}

const lock = JSON.parse(readText(join(root, "package-lock.json")));
let npmCount = 0;
for (const [path, entry] of Object.entries(lock.packages).sort(([a], [b]) =>
  a.localeCompare(b, "en"),
)) {
  if (!path || entry.dev) continue;
  const directory = join(root, path);
  if (!existsSync(join(directory, "package.json")))
    throw new Error(`请先安装锁定 npm 依赖：${path}`);
  const pkg = JSON.parse(readText(join(directory, "package.json")));
  if (pkg.version !== entry.version)
    throw new Error(`npm 安装版本与锁文件不符：${pkg.name}`);
  append("npm", pkg, directory, licenseFiles(directory));
  npmCount += 1;
}

// 独立工具链只传给本次子进程，不改用户环境；metadata 使用 offline/locked，不解析新的依赖版本。
const cargoHome = join(root, ".tools/cargo");
const localCargo = join(
  cargoHome,
  "bin",
  process.platform === "win32" ? "cargo.exe" : "cargo",
);
const metadata = JSON.parse(
  execFileSync(
    existsSync(localCargo) ? localCargo : "cargo",
    [
      "metadata",
      "--manifest-path",
      "src-tauri/Cargo.toml",
      "--format-version",
      "1",
      "--locked",
      "--offline",
      "--filter-platform",
      "x86_64-pc-windows-msvc",
    ],
    {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
      env: existsSync(localCargo)
        ? {
            ...process.env,
            CARGO_HOME: cargoHome,
            RUSTUP_HOME: join(root, ".tools/rustup"),
          }
        : process.env,
    },
  ),
);
const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
const workspace = new Set(metadata.workspace_members);
const host = metadata.packages.find(
  (pkg) => resolve(pkg.manifest_path) === join(root, "src-tauri/Cargo.toml"),
);
if (!host) throw new Error("Cargo 元数据缺少桌面宿主。");
const active = new Set();
const queue = [host.id];
while (queue.length) {
  const id = queue.pop();
  if (active.has(id)) continue;
  active.add(id);
  for (const dependency of nodes.get(id)?.deps ?? [])
    if (dependency.dep_kinds.some((edge) => edge.kind !== "dev"))
      queue.push(dependency.pkg);
}

// 以下版本发布包没有自带声明；副本来源已逐项列在 README，不将回退扩大到未知版本。
const fallbacks = new Map([
  ["alloc-stdlib 0.3.0", "alloc-stdlib-LICENSE"],
  ["selectors 0.38.0", "selectors-LICENSE-MPL-2.0"],
  ["tauri-plugin 2.7.2", "tauri-LICENSE-MIT"],
  ["webview2-com 0.39.1", "webview2-LICENSE"],
  ["webview2-com-macros 0.8.1", "webview2-LICENSE"],
  ["webview2-com-sys 0.39.1", "webview2-LICENSE"],
]);
let crateCount = 0;
for (const pkg of metadata.packages.sort((a, b) =>
  `${a.name} ${a.version}`.localeCompare(`${b.name} ${b.version}`, "en"),
)) {
  if (!active.has(pkg.id) || workspace.has(pkg.id)) continue;
  const directory = dirname(pkg.manifest_path);
  let files = licenseFiles(directory);
  const fallback = fallbacks.get(`${pkg.name} ${pkg.version}`);
  if (!files.length && fallback)
    files = [join(root, "docs/licenses", fallback)];
  append("crate", pkg, directory, files);
  crateCount += 1;
}

const output = pieces.join("");
if (check) {
  if (!existsSync(outputPath) || readText(outputPath) !== output)
    throw new Error("许可证合订本已过期，请重新运行生成器并核对差异。");
} else {
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, output, "utf8");
}
console.log(
  JSON.stringify({
    npm: npmCount,
    crates: crateCount,
    check,
    bytes: Buffer.byteLength(output),
  }),
);
