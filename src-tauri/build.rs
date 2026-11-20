// Author: ahui. Embed the frontend and Windows manifest in the release executable.
fn main() {
    // 版本号不变时也能识别重打包产物；只包含构建时间戳，不读取开发机路径或账号。
    let build_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    println!("cargo:rustc-env=QQ_BUILD_ID={build_id}");
    // Tauri 将 ICO 嵌入 EXE 并用于窗口/任务栏；单独替换图标也必须使资源重新编译。
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    // 许可由Vite作为纯文本嵌入资源；修改原文后必须重新生成EXE，避免发布包保留过期声明。
    println!("cargo:rerun-if-changed=../LICENSE");
    println!("cargo:rerun-if-changed=../THIRD_PARTY_NOTICES.md");
    println!("cargo:rerun-if-changed=../docs/licenses/THIRD_PARTY_LICENSES.txt");
    tauri_build::build();
}
