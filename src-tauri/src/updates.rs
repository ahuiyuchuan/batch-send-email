//! 公开 GitHub 发布检查与关于页外链；不读取业务状态、QQ 会话、SMTP 配置或 SQLite。
//! 手动和每小时静默检查共用进程级门闩与内存缓存，不下载、执行或替换 EXE。作者：ahui。

use chrono::Utc;
use member_core::models::{AppError, AppResult};
use reqwest::{redirect::Policy, Client, Response, StatusCode};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{future::Future, sync::Mutex, time::Duration};
use tauri::{Emitter, Manager, State};
use url::Url;

const REPOSITORY: &str = "https://github.com/ahuiyuchuan/batch-send-email";
const REPOSITORY_NAME: &str = "ahuiyuchuan/batch-send-email";
const API_REPOSITORY: &str = "https://api.github.com/repos/ahuiyuchuan/batch-send-email";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 完成后再等待一小时，失败也不密集重试；休眠错过的轮次不补跑。
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// 约束整次检查，包含 latest 与可能的仓库存在性核实，而非每请求各等一次总时限。
const CHECK_TIMEOUT: Duration = Duration::from_secs(12);
const RELEASE_LIMIT: usize = 512 * 1024;

/// 只读公开发布信息；checked_at 为成功完成检查的 UTC RFC3339 时间，失败不更新成功缓存。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateResult {
    /// available=有更高正式版，current=没有更高正式版，no-release=已核实公开仓库但无正式发布。
    status: &'static str,
    current_version: String,
    latest_version: Option<String>,
    /// 仅为本项目受控 HTTPS 发布页，不是远程执行文件或自动安装入口。
    release_url: String,
    /// GitHub body 按纯文本展示，最多4000个Unicode字符；不执行HTML/Markdown中的链接或脚本。
    notes: String,
    checked_at: String,
}

/// 请求门闩独立于成员获取、发信和数据库锁；同一时刻的界面/后台调用共享同一成功或失败。
#[derive(Default)]
pub struct UpdateCache {
    state: Mutex<CacheState>,
    request: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct CacheState {
    latest: Option<UpdateResult>,
    generation: u64,
    /// 只给正在等待上一请求的调用方复用；下一次主动检查始终可以重试。
    outcome: Option<AppResult<UpdateResult>>,
}

impl UpdateCache {
    /// 先观察代次再等门闩，等待期间已有结果即复用；失败保留最近成功缓存且只增加一次代次。
    /// 返回bool表示缓存成功内容变化，由首个执行者广播，避免并发点击产生重复事件。
    async fn check<F, R>(&self, check: F) -> AppResult<(UpdateResult, bool)>
    where
        F: FnOnce() -> R,
        R: Future<Output = AppResult<UpdateResult>>,
    {
        let generation = self.state.lock().map_err(|_| cache_error())?.generation;
        let _request = self.request.lock().await;
        {
            let state = self.state.lock().map_err(|_| cache_error())?;
            if state.generation != generation {
                if let Some(outcome) = &state.outcome {
                    return outcome.clone().map(|result| (result, false));
                }
            }
        }
        let outcome = check().await;
        let mut state = self.state.lock().map_err(|_| cache_error())?;
        let changed = outcome
            .as_ref()
            .is_ok_and(|result| state.latest.as_ref() != Some(result));
        if let Ok(result) = &outcome {
            state.latest = Some(result.clone());
        }
        state.generation = state.generation.wrapping_add(1);
        state.outcome = Some(outcome.clone());
        outcome.map(|result| (result, changed))
    }
}

fn cache_error() -> AppError {
    AppError::new("update_cache", "更新状态暂不可用，请重新打开程序后重试。")
}

/// 关于页只读最近成功检查；未完成过成功检查时返回None，不发起网络请求。
#[tauri::command]
pub fn get_update_status(state: State<'_, UpdateCache>) -> AppResult<Option<UpdateResult>> {
    state
        .state
        .lock()
        .map(|value| value.latest.clone())
        .map_err(|_| cache_error())
}

/// setup注册一次缓存并启动一次静默任务；打开/关闭关于页不会新建后台检查器。
/// 首次立即检查，成功或失败后完整等待一小时；仅成功广播，不弹通知或自动下载。
pub fn start_monitor(app: tauri::AppHandle) {
    if !app.manage(UpdateCache::default()) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        monitor_loop(|| check_updates(app.clone()), |_| {}).await;
    });
}

/// 串行完成后计时，不用interval的追赶语义；虚拟时钟测试可验证离线和长时间休眠。
async fn monitor_loop<F, R, P>(mut check: F, mut publish: P)
where
    F: FnMut() -> R,
    R: Future<Output = AppResult<UpdateResult>>,
    P: FnMut(UpdateResult),
{
    loop {
        if let Ok(result) = check().await {
            publish(result);
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// 主动检查和后台共用同一门闩；失败只返回调用者，绝不把离线或限流当作最新版。
#[tauri::command]
pub async fn check_updates(app: tauri::AppHandle) -> AppResult<UpdateResult> {
    let cache = app.state::<UpdateCache>();
    let (result, changed) = cache.check(fetch_updates).await?;
    if changed {
        let _ = app.emit("update-status-changed", &result);
    }
    Ok(result)
}

/// 固定GitHub公开API，禁重定向，无Cookie/Authorization/业务参数，兼容系统正常代理设置。
async fn fetch_updates() -> AppResult<UpdateResult> {
    let client = Client::builder()
        .redirect(Policy::none())
        .referer(false)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("batch-send-email/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AppError::new("update_client", "无法初始化更新检查，请稍后重试。"))?;
    tokio::time::timeout(
        CHECK_TIMEOUT,
        fetch_latest(
            &client,
            &format!("{API_REPOSITORY}/releases/latest"),
            API_REPOSITORY,
            CURRENT_VERSION,
        ),
    )
    .await
    .map_err(|_| AppError::new("update_timeout", "检查更新超时，请检查网络后重试。"))?
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
}

#[derive(Deserialize)]
struct RepositoryIdentity {
    full_name: String,
    private: bool,
}

/// latest只表示正式发布，无需遍历分页猜测；404还须核实同一公开仓库，私有/删除不报无发布。
/// endpoint仅为内部固定地址，测试可注入回环HTTP；生产调用方不能从IPC传入任意URL。
async fn fetch_latest(
    client: &Client,
    endpoint: &str,
    repository_endpoint: &str,
    current: &str,
) -> AppResult<UpdateResult> {
    let response = client
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|error| network_error(error, "检查更新失败，请检查网络、代理或TLS证书后重试。"))?;
    if response.status() == StatusCode::NOT_FOUND {
        let repository = client
            .get(repository_endpoint)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| network_error(error, "无法核实更新仓库，请检查网络后重试。"))?;
        ensure_success(repository.status())?;
        let identity: RepositoryIdentity = serde_json::from_slice(
            &read_limited(repository, RELEASE_LIMIT).await?,
        )
        .map_err(|_| AppError::new("update_format", "更新仓库信息格式无效，请稍后重试。"))?;
        if identity.private || !identity.full_name.eq_ignore_ascii_case(REPOSITORY_NAME) {
            return Err(AppError::new(
                "update_repository",
                "无法确认公开更新仓库，请稍后重试。",
            ));
        }
        return Ok(UpdateResult {
            status: "no-release",
            current_version: current.into(),
            latest_version: None,
            release_url: format!("{REPOSITORY}/releases"),
            notes: String::new(),
            checked_at: Utc::now().to_rfc3339(),
        });
    }
    ensure_success(response.status())?;
    parse_release(&read_limited(response, RELEASE_LIMIT).await?, current)
}

/// 不回显任意响应正文或URL；限流、权限、重定向和网络错误都有明确重试提示。
fn ensure_success(status: StatusCode) -> AppResult<()> {
    if status.is_success() {
        return Ok(());
    }
    let (code, message) = match status.as_u16() {
        403 | 429 => (
            "update_rate_limit",
            "GitHub暂时限制更新检查，请稍后重试。".into(),
        ),
        404 => (
            "update_repository",
            "更新仓库不存在或暂时不可访问，请稍后重试。".into(),
        ),
        300..=399 => (
            "update_redirect",
            "更新服务返回重定向，已停止检查。请稍后重试。".into(),
        ),
        value => (
            "update_http",
            format!("更新服务返回HTTP {value}，请稍后重试。"),
        ),
    };
    Err(AppError::new(code, message))
}

/// 请求超时与断网分别提示；底层错误可能包含代理地址，不能直接返回或写入日志。
fn network_error(error: reqwest::Error, fallback: &'static str) -> AppError {
    if error.is_timeout() {
        AppError::new("update_timeout", "检查更新超时，请检查网络后重试。")
    } else {
        AppError::new("update_network", fallback)
    }
}

/// 先检查已知长度，再流式累计512KiB上限，chunked响应也不能绕过；只读元数据不取资产。
async fn read_limited(mut response: Response, limit: usize) -> AppResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(AppError::new("update_size", "更新信息过大，已停止检查。"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            AppError::new("update_timeout", "检查更新超时，请检查网络后重试。")
        } else {
            AppError::new("update_read", "更新信息读取中断，请稍后重试。")
        }
    })? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(AppError::new("update_size", "更新信息过大，已停止检查。"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// 只接受本项目的固定HTTPS路径；拒绝脚本、本机文件、外站、查询、凭据、编码/路径绕过。
/// 返回Url仅供系统浏览器使用，永远不经cmd或PowerShell解析；不授予通用外链打开能力。
fn external_url(value: &str) -> AppResult<Url> {
    let invalid = || {
        AppError::new(
            "external_url",
            "只允许打开batch-send-email项目的GitHub页面。",
        )
    };
    if value.len() > 2048
        || value
            .chars()
            .any(|c| c <= ' ' || c == '\u{7f}' || c == '\\' || c == '%')
        || !value.starts_with("https://github.com/")
        || value.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let path = url.path();
    let base = "/ahuiyuchuan/batch-send-email";
    let allowed = [
        base.to_string(),
        format!("{base}/"),
        format!("{base}/releases"),
        format!("{base}/issues"),
        format!("{base}/issues/new"),
        format!("{base}/blob/main/LICENSE"),
    ];
    if allowed.iter().any(|allowed| allowed == path) {
        return Ok(url);
    }
    if let Some(tag) = path.strip_prefix(&format!("{base}/releases/tag/")) {
        if let Ok(version) = Version::parse(tag.strip_prefix('v').unwrap_or(tag)) {
            if version.pre.is_empty() {
                return Ok(url);
            }
        }
    }
    Err(invalid())
}

/// 原生浏览器入口仍校验白名单，ShellExecuteW不接受可执行参数，失败不影响邮件/成员任务。
#[tauri::command]
pub fn open_external_url(url: String) -> AppResult<()> {
    let url = external_url(&url)?;
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                window: *mut c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> *mut c_void;
        }
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let target: Vec<u16> = url.as_str().encode_utf16().chain(Some(0)).collect();
        // 两个UTF16缓冲区在调用期间有效且NUL终止；不经过shell字符串拼接。
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if result as isize <= 32 {
            return Err(AppError::new(
                "external_open",
                "无法打开系统默认浏览器，请检查系统关联。",
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err(AppError::new(
            "external_open",
            "请在Windows桌面程序中打开项目页面。",
        ))
    }
}

/// 语义版本按precedence比较，构建元信息不触发更新；标签与本项目发布路径必须完全对应。
fn parse_release(bytes: &[u8], current: &str) -> AppResult<UpdateResult> {
    let release: Release = serde_json::from_slice(bytes)
        .map_err(|_| AppError::new("update_format", "更新信息格式无效，请稍后重试。"))?;
    let latest = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )
    .map_err(|_| AppError::new("update_version", "发布版本号格式无效，请查看发布记录。"))?;
    let current = Version::parse(current)
        .map_err(|_| AppError::new("update_version", "当前版本号格式无效，请重新下载正式版本。"))?;
    if release.draft || release.prerelease || !latest.pre.is_empty() {
        return Err(AppError::new(
            "update_release",
            "更新服务未返回正式发布版本，请查看发布记录。",
        ));
    }
    let page = external_url(&release.html_url)?;
    if page.as_str() != format!("{REPOSITORY}/releases/tag/{}", release.tag_name) {
        return Err(AppError::new(
            "update_release",
            "发布标签与页面不匹配，请查看发布记录。",
        ));
    }
    Ok(UpdateResult {
        status: if latest.cmp_precedence(&current).is_gt() {
            "available"
        } else {
            "current"
        },
        current_version: current.to_string(),
        latest_version: Some(latest.to_string()),
        release_url: page.into(),
        notes: release
            .body
            .unwrap_or_default()
            .chars()
            .take(4000)
            .collect(),
        checked_at: Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 发布后显式运行真实公开API闭环；常规测试忽略此项，不受外网和GitHub限流影响。
    /// 此测试不使用认证、不读取应用数据，只核对当前正式版本已可被生产检查器识别。
    #[tokio::test]
    #[ignore = "requires public GitHub access and a published matching release"]
    async fn live_public_release() {
        let result = fetch_updates().await.unwrap();
        assert_eq!(result.status, "current");
        assert_eq!(result.current_version, CURRENT_VERSION);
        assert_eq!(result.latest_version.as_deref(), Some(CURRENT_VERSION));
        assert_eq!(
            result.release_url,
            format!("{REPOSITORY}/releases/tag/v{CURRENT_VERSION}")
        );
    }
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn available() -> UpdateResult {
        UpdateResult {
            status: "available",
            current_version: "1.0.0".into(),
            latest_version: Some("1.1.0".into()),
            release_url: format!("{REPOSITORY}/releases/tag/v1.1.0"),
            notes: "合成发布说明".into(),
            checked_at: "2026-11-17T00:00:00Z".into(),
        }
    }

    /// 并行手动/后台检查只执行一次网络工作，只有首个执行者发布；后续主动点击仍可重查。
    #[tokio::test(start_paused = true)]
    async fn overlapping_checks_share_one_request_and_publish_once() {
        let cache = UpdateCache::default();
        let calls = AtomicUsize::new(0);
        let check = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(available())
        };
        let (first, second, third) =
            tokio::join!(cache.check(check), cache.check(check), cache.check(check));
        assert!(first.unwrap().1);
        assert!(!second.unwrap().1);
        assert!(!third.unwrap().1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!cache.check(check).await.unwrap().1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// 失败共享给等待者但不改旧成功缓存；不能因缓存失败而永久阻止用户重试。
    #[tokio::test(start_paused = true)]
    async fn failed_check_preserves_old_success_and_later_retry_recovers() {
        let cache = UpdateCache::default();
        cache.check(|| async { Ok(available()) }).await.unwrap();
        let calls = AtomicUsize::new(0);
        let check = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(1)).await;
            Err(AppError::new("update_network", "合成离线错误"))
        };
        let (first, second) = tokio::join!(cache.check(check), cache.check(check));
        assert!(first.is_err());
        assert!(second.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(cache.state.lock().unwrap().latest, Some(available()));
        cache.check(|| async { Ok(available()) }).await.unwrap();
        assert!(cache
            .state
            .lock()
            .unwrap()
            .outcome
            .as_ref()
            .unwrap()
            .is_ok());
    }

    /// 首次立即检查，失败也完整等一小时；休眠跨多小时恢复仅重查一次，不追赶旧轮次。
    #[tokio::test(start_paused = true)]
    async fn monitor_waits_after_failure_and_skips_missed_intervals() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let published = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let notices = published.clone();
        let worker = tokio::spawn(monitor_loop(
            move || {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                std::future::ready(if index == 0 {
                    Err(AppError::new("update_network", "合成离线错误"))
                } else {
                    Ok(available())
                })
            },
            move |_| {
                notices.fetch_add(1, Ordering::SeqCst);
            },
        ));
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(published.load(Ordering::SeqCst), 0);
        tokio::time::advance(CHECK_INTERVAL - Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(published.load(Ordering::SeqCst), 1);
        tokio::time::advance(CHECK_INTERVAL * 12).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        worker.abort();
    }

    /// 白名单仅限本项目固定路由及正式版本标签，外站、编码/路径绕过和任意本机协议均拒绝。
    #[test]
    fn external_browser_only_accepts_project_pages() {
        for suffix in [
            "",
            "/",
            "/releases",
            "/issues",
            "/issues/new",
            "/blob/main/LICENSE",
            "/releases/tag/v1.1.0",
            "/releases/tag/1.1.0+build.2",
        ] {
            assert!(
                external_url(&format!("{REPOSITORY}{suffix}")).is_ok(),
                "{suffix}"
            );
        }
        for value in [
            "javascript:alert(1)",
            "file:///C:/test.exe",
            "shell:AppsFolder",
            "//github.com/ahuiyuchuan/batch-send-email",
            "http://github.com/ahuiyuchuan/batch-send-email",
            "https://github.com.evil.test/ahuiyuchuan/batch-send-email",
            "https://github.com/ahuiyuchuan/another-project",
            "https://a:b@github.com/ahuiyuchuan/batch-send-email",
            "https://github.com:8443/ahuiyuchuan/batch-send-email",
            "https://github.com/ahuiyuchuan/batch-send-email?token=x",
            "https://github.com/ahuiyuchuan/batch-send-email#readme",
            "https://github.com/ahuiyuchuan/batch-send-email/releases/latest/download/test.exe",
            "https://github.com/ahuiyuchuan/batch-send-email/releases/tag/v1.0.0-beta.1",
            "https://github.com/ahuiyuchuan/batch-send-email/%2e%2e/other",
            "https://github.com/ahuiyuchuan/batch-send-email/../batch-send-email",
            "https://github.com/ahuiyuchuan/batch-send-email\\issues",
            "https://github.com/ahuiyuchuan/batch-send-email\n",
        ] {
            assert!(external_url(value).is_err(), "{value}");
        }
    }

    fn release_json(tag: &str) -> serde_json::Value {
        json!({"tag_name":tag,"html_url":format!("{REPOSITORY}/releases/tag/{tag}"),"draft":false,"prerelease":false,"body":"合成公开说明"})
    }

    /// 1.10大于1.9，当前较新时不降级；构建号变化无更新，发布链接必须对应同一标签。
    #[test]
    fn semantic_versions_and_matching_release_route_are_required() {
        let value = release_json("v1.10.0");
        for (current, expected) in [
            ("1.9.0", "available"),
            ("1.10.0", "current"),
            ("2.0.0", "current"),
        ] {
            let result = parse_release(value.to_string().as_bytes(), current).unwrap();
            assert_eq!(result.status, expected);
            assert!(chrono::DateTime::parse_from_rfc3339(&result.checked_at).is_ok());
        }
        assert_eq!(
            parse_release(
                release_json("v1.0.0+build.2").to_string().as_bytes(),
                "1.0.0+build.1"
            )
            .unwrap()
            .status,
            "current"
        );
        let mut value = release_json("v1.10.0");
        for field in ["prerelease", "draft"] {
            value[field] = json!(true);
            assert!(parse_release(value.to_string().as_bytes(), "1.0.0").is_err());
            value[field] = json!(false);
        }
        value["html_url"] = json!(format!("{REPOSITORY}/releases/tag/v1.9.0"));
        assert!(parse_release(value.to_string().as_bytes(), "1.0.0").is_err());
        for tag in ["v1.0.1-beta.1", "v1.01.0", "not-a-version"] {
            assert!(parse_release(release_json(tag).to_string().as_bytes(), "1.0.0").is_err());
        }
        assert!(parse_release(b"not-json", "1.0.0").is_err());
        let mut value = release_json("v1.1.0");
        value["body"] = json!("中".repeat(5000));
        assert_eq!(
            parse_release(value.to_string().as_bytes(), "1.0.0")
                .unwrap()
                .notes
                .chars()
                .count(),
            4000
        );
    }

    /// 简单HTTP夹具每个连接只回一个预设响应；不读取QQ或SMTP，也不请求外部服务。
    fn mock_server(responses: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut input = [0; 4096];
                let count = stream.read(&mut input).unwrap();
                let request = String::from_utf8_lossy(&input[..count]).to_lowercase();
                assert!(request.starts_with("get "));
                assert!(!request.contains("authorization:"));
                assert!(!request.contains("cookie:"));
                // 过大响应可在客户端读完前被主动关闭，BrokenPipe属于预期的限制路径。
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (base, worker)
    }

    fn response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn test_client() -> Client {
        Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
    }

    /// 只有latest404且仓库身份公开且匹配才是no-release；不存在/身份错误/限流不能冒充最新版。
    #[tokio::test]
    async fn missing_latest_requires_verified_public_repository() {
        for (status, body, expected) in [
            (
                "200 OK",
                json!({"full_name":REPOSITORY_NAME,"private":false}).to_string(),
                true,
            ),
            (
                "200 OK",
                json!({"full_name":REPOSITORY_NAME,"private":true}).to_string(),
                false,
            ),
            (
                "200 OK",
                json!({"full_name":"other/repository","private":false}).to_string(),
                false,
            ),
            ("200 OK", "{}".into(), false),
            ("404 Not Found", "{}".into(), false),
            ("429 Too Many Requests", "{}".into(), false),
        ] {
            let (base, worker) = mock_server(vec![
                response("404 Not Found", "{}"),
                response(status, &body),
            ]);
            let result =
                fetch_latest(&test_client(), &format!("{base}/latest"), &base, "1.0.0").await;
            assert_eq!(result.is_ok(), expected);
            if let Ok(result) = result {
                assert_eq!(result.status, "no-release");
                assert_eq!(result.latest_version, None);
            }
            worker.join().unwrap();
        }
    }

    /// 真实HTTP读取正式版本；HTTP错误不解析为发布，302不跟随，超大声明不占用无界内存。
    #[tokio::test]
    async fn http_checks_release_status_redirect_and_declared_size_boundaries() {
        for (wire, expected) in [
            (response("200 OK", &release_json("v1.1.0").to_string()), "available"),
            (response("403 Forbidden", "sensitive server message"), "update_rate_limit"),
            (response("429 Too Many Requests", "{}"), "update_rate_limit"),
            ("HTTP/1.1 302 Found\r\nLocation: https://evil.test/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), "update_redirect"),
            (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", RELEASE_LIMIT+1), "update_size"),
        ] {
            let (base, worker) = mock_server(vec![wire]);
            let result = fetch_latest(&test_client(), &base, &base, "1.0.0").await;
            match result { Ok(value)=>assert_eq!(value.status,expected),Err(error)=>assert_eq!(error.code,expected) }
            worker.join().unwrap();
        }
    }

    /// 没有Content-Length的chunked超量仍拒绝；合法大小不会受非ASCII多字节内容影响。
    #[tokio::test]
    async fn chunked_body_cannot_bypass_stream_limit() {
        let body = "x".repeat(RELEASE_LIMIT + 1);
        let wire = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
        let (base, worker) = mock_server(vec![wire]);
        let result = fetch_latest(&test_client(), &base, &base, "1.0.0")
            .await
            .unwrap_err();
        assert_eq!(result.code, "update_size");
        worker.join().unwrap();
    }

    /// 本机服务不返回HTTP头时，真实客户端按有限预算退出；错误不带地址或任意底层文本。
    #[tokio::test]
    async fn slow_http_response_returns_actionable_timeout() {
        use std::{io::Read, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = [0; 4096];
            let count = stream.read(&mut input).unwrap();
            assert!(count > 0);
            std::thread::sleep(Duration::from_millis(500));
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(250))
            .build()
            .unwrap();
        let error = fetch_latest(&client, &endpoint, &endpoint, "1.0.0")
            .await
            .unwrap_err();
        assert_eq!(error.code, "update_timeout");
        assert!(!error.message.contains("127.0.0.1"));
        worker.join().unwrap();
    }
}
