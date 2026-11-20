//! QQ 网页扫码、会话校验与成员读取；网络行为统一按受限接口流程处理。
//! Cookie 保留原始域、路径与服务端到期时间；页面只能取得二维码和状态，不能读取登录凭据。
//! 作者：ahui。

use super::shared::{
    cancelled_error, count, identifier, is_numeric_id, network_error, response_bytes,
    response_json, schema_error, text, REQUEST_TIMEOUT,
};
use crate::diagnostics;
use crate::models::{
    Account, AppError, AppResult, FetchResult, FriendList, Group, LoginPoll, Member,
    MemberProvider, Progress, QrCode,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{cookie::CookieStore, header::HeaderValue, redirect::Policy, Client};
use serde_json::Value;
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, RwLock as StdRwLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use url::Url;

const APP_ID: &str = "715030901";
const DOMAIN_ID: &str = "73";
const MEMBER_PAGE: &str = "https://qun.qq.com/member.html";
const LOGIN_LIFETIME: Duration = Duration::from_secs(180);
const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// 好友列表只在本次任务内使用；较小的独立上限防止异常响应占用群名单的通用预算。
const MAX_FRIEND_LIST_BYTES: usize = 2 * 1024 * 1024;
/// reqwest CookieStore 的最小实现；避免第三方默认 Cookie 调试日志记录秘密值。
#[derive(Default)]
struct SessionCookies(StdRwLock<cookie_store::CookieStore>);

impl CookieStore for SessionCookies {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let mut store = self.0.write().unwrap_or_else(|p| p.into_inner());
        for header in headers {
            if let Ok(raw) = header.to_str() {
                let _ = store.parse(raw, url);
            }
        }
    }
    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let store = self.0.read().unwrap_or_else(|p| p.into_inner());
        let value = store
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if value.is_empty() {
            None
        } else {
            HeaderValue::from_str(&value).ok()
        }
    }
}

// 固定常规 UA 只服务于 QQ 网页协议，不用于绕过验证码或设备验证。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

/// 单次扫码的隔离会话。取消后即使网络请求完成，也不能重新成为活动会话。
struct Session {
    generation: u64,
    client: Client,
    jar: Arc<SessionCookies>,
    started: Instant,
    xlogin: Url,
    authenticated: AtomicBool,
    nickname: RwLock<String>,
    poll_lock: Mutex<()>,
    /// 只记录扫码状态变化；正常的两秒轮询不逐次写日志，避免等待扫码时刷屏。
    last_login_status: AtomicU8,
}

/// 使用自己已加入群的账号扫码读取成员；实际权限由 QQ 服务端决定。
///
/// 重新扫码建立全新内存 Cookie Store；不保存到磁盘，关闭程序即释放。不能调试输出凭据。
pub struct WebProvider {
    session: RwLock<Option<Arc<Session>>>,
    generation: AtomicU64,
}

impl WebProvider {
    /// 创建未登录的网页来源。不会访问网络或读取系统浏览器的已有 Cookie。
    pub fn new() -> AppResult<Self> {
        Ok(Self {
            session: RwLock::new(None),
            generation: AtomicU64::new(0),
        })
    }

    /// 获取新的扫码图片，旧二维码和旧会话立即失效。
    ///
    /// 成功仅表示二维码已生成；仍需轮询并由用户在手机 QQ 上确认。
    /// 网络错误、图片格式异常和缺少 qrsig 时返回安全错误，不把 Cookie 暴露到页面。
    pub async fn start_login(&self) -> AppResult<QrCode> {
        let mut diagnostic = diagnostics::operation("qq_web", "start_login");
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let jar = Arc::new(SessionCookies::default());
        let client = session_client(jar.clone())?;
        let mut xlogin = parse_fixed_url("https://xui.ptlogin2.qq.com/cgi-bin/xlogin")?;
        xlogin.query_pairs_mut().extend_pairs([
            ("pt_disable_pwd", "1"),
            ("appid", APP_ID),
            ("daid", DOMAIN_ID),
            ("hide_close_icon", "1"),
            ("pt_no_auth", "1"),
            ("s_url", MEMBER_PAGE),
        ]);
        let session = Arc::new(Session {
            generation,
            client,
            jar,
            started: Instant::now(),
            xlogin,
            authenticated: AtomicBool::new(false),
            nickname: RwLock::new(String::new()),
            poll_lock: Mutex::new(()),
            last_login_status: AtomicU8::new(0),
        });
        {
            let mut active = self.session.write().await;
            self.ensure_current(&session)?;
            *active = Some(session.clone());
        }

        request_bytes(
            "login_page",
            session.client.get(session.xlogin.clone()),
            MAX_IMAGE_BYTES,
            false,
        )
        .await?;
        self.ensure_current(&session)?;

        let mut image_url = parse_fixed_url("https://ssl.ptlogin2.qq.com/ptqrshow")?;
        let nonce = epoch_millis().to_string();
        image_url.query_pairs_mut().extend_pairs([
            ("appid", APP_ID),
            ("e", "2"),
            ("l", "M"),
            ("s", "3"),
            ("d", "72"),
            ("v", "4"),
            ("t", nonce.as_str()),
            ("daid", DOMAIN_ID),
            ("pt_3rd_aid", "0"),
        ]);
        let bytes = request_bytes(
            "qr_image",
            session.client.get(image_url.clone()),
            MAX_IMAGE_BYTES,
            false,
        )
        .await?;
        self.ensure_current(&session)?;
        if cookie(&session.jar, &image_url, "qrsig").is_none() {
            return Err(AppError::new(
                "qr_unavailable",
                "QQ 未能生成有效二维码，请稍后刷新二维码。",
            ));
        }
        let mime = image_mime(&bytes).ok_or_else(|| {
            AppError::new(
                "qr_unavailable",
                "QQ 返回的二维码格式不受支持，请稍后重试。",
            )
        })?;
        diagnostics::event(
            "info",
            "qq_web",
            "qr_ready",
            serde_json::json!({"image_bytes": bytes.len(), "lifetime_seconds": LOGIN_LIFETIME.as_secs()}),
        );
        diagnostic.finish("success");
        Ok(QrCode {
            image_data_url: format!("data:{mime};base64,{}", STANDARD.encode(bytes)),
            expires_in: LOGIN_LIFETIME.as_secs() as u32,
        })
    }

    /// 查询当前二维码状态。一个实例同时只轮询一次，避免重复确认跳转。
    ///
    /// success 仅在 QQ 回调、受控登录跳转、账号 Cookie 及群列表验证全部通过后返回。
    pub async fn poll_login(&self) -> AppResult<LoginPoll> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(login_required)?;
        let _poll_guard = session.poll_lock.lock().await;
        self.ensure_current(&session)?;
        if session.authenticated.load(Ordering::SeqCst) {
            return Ok(observed_poll(&session, "success", "登录成功。"));
        }
        if session.started.elapsed() > LOGIN_LIFETIME {
            return Ok(observed_poll(
                &session,
                "expired",
                "二维码已过期，请刷新二维码。",
            ));
        }

        let mut poll_url = parse_fixed_url("https://ssl.ptlogin2.qq.com/ptqrlogin")?;
        let qrsig = cookie(&session.jar, &poll_url, "qrsig")
            .ok_or_else(|| AppError::new("qr_expired", "二维码已失效，请刷新二维码。"))?;
        let signature = cookie(&session.jar, &session.xlogin, "pt_login_sig").unwrap_or_default();
        let token = hash33(&qrsig, 0).to_string();
        let action = format!("0-0-{}", epoch_millis());
        poll_url.query_pairs_mut().extend_pairs([
            ("u1", MEMBER_PAGE),
            ("ptqrtoken", token.as_str()),
            ("ptredirect", "1"),
            ("h", "1"),
            ("t", "1"),
            ("g", "1"),
            ("from_ui", "1"),
            ("ptlang", "2052"),
            ("action", action.as_str()),
            ("js_ver", "26071711"),
            ("js_type", "1"),
            ("login_sig", signature.as_str()),
            ("pt_uistyle", "40"),
            ("aid", APP_ID),
            ("daid", DOMAIN_ID),
        ]);
        let bytes = request_bytes(
            "qr_poll",
            session
                .client
                .get(poll_url)
                .header(reqwest::header::REFERER, session.xlogin.as_str()),
            MAX_IMAGE_BYTES,
            true,
        )
        .await?;
        self.ensure_current(&session)?;
        let body = std::str::from_utf8(&bytes).map_err(|_| schema_error())?;
        let arguments = callback_arguments(body)?;
        match arguments.first().map(String::as_str) {
            Some("66") => Ok(observed_poll(&session, "wait", "请使用手机 QQ 扫码")),
            Some("67") => Ok(observed_poll(
                &session,
                "scanned",
                "已扫码，请在手机上确认登录",
            )),
            Some("65") => Ok(observed_poll(&session, "expired", "二维码已过期，请刷新")),
            Some("0") => {
                let redirect = arguments.get(2).ok_or_else(schema_error)?;
                let target = Url::parse(redirect).map_err(|_| schema_error())?;
                if !qq_https_url(&target) {
                    return Err(AppError::new(
                        "redirect_rejected",
                        "登录跳转不受支持，请刷新二维码重试。",
                    ));
                }
                request_bytes(
                    "login_redirect",
                    session.client.get(target),
                    MAX_IMAGE_BYTES,
                    false,
                )
                .await?;
                self.ensure_current(&session)?;
                request_bytes(
                    "member_page",
                    member_page_request(&session),
                    MAX_IMAGE_BYTES,
                    false,
                )
                .await?;
                self.ensure_current(&session)?;

                // 不能只相信登录回调的成功码；有效身份和已登录的群接口还需要服务端核实。
                session_account_id(&session)?;
                groups_for(&session).await?;
                self.ensure_current(&session)?;
                *session.nickname.write().await = arguments.get(5).cloned().unwrap_or_default();
                session.authenticated.store(true, Ordering::SeqCst);
                self.ensure_current(&session)?;
                Ok(observed_poll(
                    &session,
                    "success",
                    "登录成功，可以选择已加入的群。",
                ))
            }
            _ => Err(AppError::new(
                "login_verification",
                "QQ 暂时无法完成扫码登录，请在手机确认后刷新二维码重试。",
            )),
        }
    }

    /// 取消尚未完成的扫码；已登录会话保留，登出由宿主丢弃整个来源实例完成。
    pub async fn cancel_login(&self) {
        let mut active = self.session.write().await;
        if active
            .as_ref()
            .is_none_or(|session| !session.authenticated.load(Ordering::SeqCst))
        {
            self.generation.fetch_add(1, Ordering::SeqCst);
            *active = None;
            diagnostics::event("info", "qq_web", "qr_cancelled", serde_json::json!({}));
        }
    }

    /// 克隆当前有效会话；网络等待期间不持有全局锁。
    async fn authenticated_session(&self) -> AppResult<Arc<Session>> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or_else(login_required)?;
        self.ensure_current(&session)?;
        if !session.authenticated.load(Ordering::SeqCst) {
            return Err(login_required());
        }
        Ok(session)
    }

    /// 代次检查使取消、刷新二维码或账号变化后的迟到请求失效。
    fn ensure_current(&self, session: &Session) -> AppResult<()> {
        if self.generation.load(Ordering::SeqCst) != session.generation {
            return Err(cancelled_error());
        }
        Ok(())
    }

    /// 好友关系只属于请求开始时的账号；取消或 Cookie 身份变化后不能继续使用迟到结果。
    ///
    /// 读取号码只使用当前隔离会话的 Cookie，不读取浏览器配置，也不把账号或凭据写入日志。
    fn ensure_friend_account(
        &self,
        session: &Session,
        expected_account: &str,
        cancel: &CancellationToken,
    ) -> AppResult<()> {
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        self.ensure_current(session)?;
        if session_account_id(session)? != expected_account {
            return Err(AppError::new(
                "account_changed",
                "读取好友期间登录账号已变化，请重新扫码登录后获取名单。",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl MemberProvider for WebProvider {
    fn source(&self) -> &'static str {
        "web"
    }

    async fn account(&self) -> AppResult<Account> {
        let session = self.authenticated_session().await?;
        let qq_number = session_account_id(&session)?;
        // 用实时的群接口验证 Cookie 仍有效；旧账号缓存不能代替当前登录状态。
        groups_for(&session).await?;
        self.ensure_current(&session)?;
        // QQ 可能在响应中轮换 Cookie；身份必须与请求开始时一致，不能把旧账号配到新凭据。
        if session_account_id(&session)? != qq_number {
            return Err(AppError::new(
                "account_changed",
                "验证期间登录账号已变化，请重新扫码登录。",
            ));
        }
        let nickname = session.nickname.read().await.clone();
        Ok(Account {
            qq_number,
            nickname,
            source: self.source().into(),
        })
    }

    async fn groups(&self) -> AppResult<Vec<Group>> {
        let session = self.authenticated_session().await?;
        let groups = groups_for(&session).await?;
        self.ensure_current(&session)?;
        Ok(groups)
    }

    /// 仅在宿主显式请求排除好友时读取一次；不缓存或持久化整个好友列表。
    ///
    /// 成功结果以本次网页会话的账号为边界，名单解析失败、部分响应、取消及账号变化均返回错误。
    /// 不回退到群接口的 `fr` 字段，避免不同来源静默改变“排除好友”的含义。
    async fn friends(&self, cancel: CancellationToken) -> AppResult<FriendList> {
        let mut diagnostic = diagnostics::operation("qq_web", "fetch_friends");
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        let session = self.authenticated_session().await?;
        let qq_number = session_account_id(&session)?;
        self.ensure_friend_account(&session, &qq_number, &cancel)?;
        let request = friend_list_request(&session)?;
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled_error()),
            result = request_bytes("friends", request, MAX_FRIEND_LIST_BYTES, false) => result?,
        };
        self.ensure_friend_account(&session, &qq_number, &cancel)?;
        let qq_numbers = parse_friend_list_bytes(&bytes)?;
        let nickname = session.nickname.read().await.clone();
        self.ensure_friend_account(&session, &qq_number, &cancel)?;
        diagnostics::event(
            "info",
            "qq_web",
            "friends_received",
            serde_json::json!({"friends": qq_numbers.len(), "response_bytes": bytes.len()}),
        );
        diagnostic.finish("success");
        Ok(FriendList {
            account: Account {
                qq_number,
                nickname,
                source: self.source().into(),
            },
            qq_numbers,
        })
    }

    async fn fetch(
        &self,
        group: &Group,
        progress: Progress,
        cancel: CancellationToken,
    ) -> AppResult<FetchResult> {
        let mut diagnostic = diagnostics::operation("qq_web", "fetch_members");
        if !is_numeric_id(&group.id) {
            return Err(AppError::new(
                "invalid_group",
                "请选择当前账号已加入的有效群。",
            ));
        }
        let session = self.authenticated_session().await?;
        progress("正在核实群成员资格…".into());
        let current_groups = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_error()),
            result = groups_for(&session) => result?,
        };
        let before = current_groups
            .iter()
            .find(|item| item.id == group.id)
            .ok_or_else(|| {
                AppError::new(
                    "group_unavailable",
                    "当前账号已无法访问该群，请刷新群列表后重新选择。",
                )
            })?;
        progress("正在读取群成员，等待 QQ 返回名单…".into());
        let mut url = parse_fixed_url("https://qun.qq.com/cgi-bin/qun_info/get_members_info_v1")?;
        let bkn = session_bkn(&session)?.to_string();
        url.query_pairs_mut().extend_pairs([
            ("gc", group.id.as_str()),
            ("friends", "1"),
            ("name", "1"),
            ("bkn", bkn.as_str()),
        ]);
        let request = web_request(&session, url);
        let value = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_error()),
            result = send_json("members", request) => result?,
        };
        self.ensure_current(&session)?;
        check_web_response(&value)?;
        let mut result = parse_members(&value)?;

        // 群列表与名单分别取自实时请求；若可见人数不同，结构化标记为待核验。
        if before
            .member_count
            .zip(result.reported_count)
            .is_some_and(|(a, b)| a != b)
        {
            result.freshness_confirmed = false;
            result
                .warnings
                .push("获取前后的参考人数不同，群成员可能正在变化，请重新核验。".into());
        }
        progress("正在复核群人数与结果…".into());
        let after = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_error()),
            result = groups_for(&session) => result?,
        };
        self.ensure_current(&session)?;
        let after_group = after
            .iter()
            .find(|item| item.id == group.id)
            .ok_or_else(|| {
                AppError::new(
                    "group_unavailable",
                    "获取期间该群已不可访问，请刷新群列表。",
                )
            })?;
        if before
            .member_count
            .zip(after_group.member_count)
            .is_some_and(|(a, b)| a != b)
            || result
                .reported_count
                .zip(after_group.member_count)
                .is_some_and(|(a, b)| a != b)
        {
            result.freshness_confirmed = false;
            result
                .warnings
                .push("读取期间群人数发生变化，本次名单需要重新核验。".into());
        }
        diagnostics::event(
            "info",
            "qq_web",
            "members_received",
            serde_json::json!({"group": diagnostics::correlate(&group.id), "pages": 1, "members": result.members.len(), "reported": result.reported_count, "freshness_confirmed": result.freshness_confirmed, "warnings": result.warnings.len()}),
        );
        diagnostic.finish("success");
        Ok(result)
    }
}

/// 扫码确认后访问固定 HTTPS 成员页，不接受来自页面的任意 URL。
fn member_page_request(session: &Session) -> reqwest::RequestBuilder {
    session.client.get(MEMBER_PAGE)
}

/// 创建只允许 HTTPS QQ 跳转的内存连接，不复用系统浏览器凭据。
fn session_client(jar: Arc<SessionCookies>) -> AppResult<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .cookie_provider(jar)
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 || !qq_https_url(attempt.url()) {
                attempt.error("untrusted login redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| AppError::new("client_error", "无法初始化登录连接，请重新启动程序。"))
}

/// Unix 秒只决定固定本地截止时间，不延长 QQ 服务端凭据的期限。
async fn groups_for(session: &Session) -> AppResult<Vec<Group>> {
    let bkn = session_bkn(session)?.to_string();
    let request = web_request(
        session,
        parse_fixed_url("https://qun.qq.com/cgi-bin/qun_mgr/get_group_list")?,
    )
    .form(&[("bkn", bkn.as_str())]);
    let value = send_json("groups", request).await?;
    check_web_response(&value)?;
    parse_groups(&value)
}

/// 按网页接口使用当前会话动态计算 bkn；查询与表单都带相同值，不使用外部传入的 Cookie 或令牌。
///
/// 2026-11-05 核对的 QQ 公开构建 7904f18b 在以下文件中以空业务参数调用该接口；只参考调用约定。
/// https://qq-social-web.cdn-go.cn/qq-qun-web/7904f18b/assets/index.85819934.js
fn friend_list_request(session: &Session) -> AppResult<reqwest::RequestBuilder> {
    let bkn = session_bkn(session)?.to_string();
    let timestamp = epoch_millis().to_string();
    let mut url = parse_fixed_url("https://qun.qq.com/cgi-bin/qun_mgr/get_friend_list")?;
    url.query_pairs_mut()
        .extend_pairs([("bkn", bkn.as_str()), ("ts", timestamp.as_str())]);
    Ok(web_request(session, url).form(&[("bkn", bkn.as_str())]))
}

/// 使用固定 QQ origin 和来源页面；凭据由原始域及路径规则自动选择，不转发到其他域。
fn web_request(session: &Session, url: Url) -> reqwest::RequestBuilder {
    let request = if matches!(
        url.path(),
        "/cgi-bin/qun_mgr/get_group_list" | "/cgi-bin/qun_mgr/get_friend_list"
    ) {
        session.client.post(url)
    } else {
        session.client.get(url)
    };
    request
        .header(reqwest::header::ORIGIN, "https://qun.qq.com")
        .header(reqwest::header::REFERER, MEMBER_PAGE)
        .header("X-Requested-With", "XMLHttpRequest")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/javascript, */*; q=0.01",
        )
}

async fn send_json(endpoint: &'static str, request: reqwest::RequestBuilder) -> AppResult<Value> {
    let started = Instant::now();
    let response = request
        .send()
        .await
        .map_err(|error| request_error(endpoint, started, error))?;
    let status = response.status().as_u16();
    let result = response_json(response).await;
    diagnostics::event(
        if result.is_ok() { "info" } else { "warn" },
        "qq_web",
        "http_completed",
        serde_json::json!({"endpoint": endpoint, "status": status, "duration_ms": started.elapsed().as_millis() as u64, "outcome": if result.is_ok() { "success" } else { "response_error" }, "error_code": result.as_ref().err().map(|error| error.code.as_str())}),
    );
    result
}

/// 网络事件只使用源码固定端点标识，不读取 Request/Response 的 URL、Cookie、Header 或正文。
async fn request_bytes(
    endpoint: &'static str,
    request: reqwest::RequestBuilder,
    maximum: usize,
    quiet_success: bool,
) -> AppResult<Vec<u8>> {
    let started = Instant::now();
    let response = request
        .send()
        .await
        .map_err(|error| request_error(endpoint, started, error))?;
    let status = response.status().as_u16();
    let result = response_bytes(response, maximum).await;
    if !quiet_success || result.is_err() {
        diagnostics::event(
            if result.is_ok() { "info" } else { "warn" },
            "qq_web",
            "http_completed",
            serde_json::json!({"endpoint": endpoint, "status": status, "duration_ms": started.elapsed().as_millis() as u64, "outcome": if result.is_ok() { "success" } else { "response_error" }, "error_code": result.as_ref().err().map(|error| error.code.as_str())}),
        );
    }
    result
}

/// 不调用 reqwest::Error 的 Display/Debug；其中可能含登录签名与重定向 URL。
fn request_error(endpoint: &'static str, started: Instant, error: reqwest::Error) -> AppError {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else {
        "request"
    };
    diagnostics::event(
        "warn",
        "qq_web",
        "http_failed",
        serde_json::json!({"endpoint": endpoint, "kind": kind, "duration_ms": started.elapsed().as_millis() as u64, "status": error.status().map(|status| status.as_u16())}),
    );
    network_error(error)
}

/// 一个二维码只在状态改变时记一次；等待期间重复返回不会产生诊断噪声。
fn observed_poll(session: &Session, status: &'static str, message: &str) -> LoginPoll {
    let state = match status {
        "wait" => 1,
        "scanned" => 2,
        "expired" => 3,
        "success" => 4,
        _ => 5,
    };
    if session.last_login_status.swap(state, Ordering::Relaxed) != state {
        diagnostics::event(
            "info",
            "qq_web",
            "qr_state_changed",
            serde_json::json!({"status": status, "generation": session.generation}),
        );
    }
    login_poll(status, message)
}

/// 仅识别已知登录/权限错误；远端 em/msg 不直接转交界面，避免泄露正文或脚本。
fn check_web_response(value: &Value) -> AppResult<()> {
    if !value.is_object() {
        return Err(schema_error());
    }
    let status = value.get("ec").or_else(|| value.get("retcode"));
    if status.is_some_and(|value| {
        !value.is_null() && !value.is_string() && !value.is_i64() && !value.is_u64()
    }) {
        return Err(schema_error());
    }
    let code = identifier(status);
    let result = match code.as_str() {
        "" | "0" => Ok(()),
        "4" => Err(AppError::new(
            "login_expired",
            "QQ 登录已过期，请重新扫码登录。",
        )),
        "7" => Err(AppError::new(
            "group_forbidden",
            "当前账号不在该群或 QQ 不允许读取，请刷新群列表并核对当前账号权限。",
        )),
        _ => Err(AppError::new(
            "qq_api_error",
            "QQ 接口暂时无法完成请求，请稍后重试。",
        )),
    };
    if let Err(error) = &result {
        // 只保留可解析的整数业务码与本机固定分类；远端字符串、消息和响应正文永不进入日志。
        diagnostics::event(
            "warn",
            "qq_web",
            "business_rejected",
            serde_json::json!({"business_code": code.parse::<i64>().ok(), "error_code": error.code}),
        );
    }
    result
}

fn parse_groups(value: &Value) -> AppResult<Vec<Group>> {
    let mut groups = Vec::new();
    let mut seen = HashSet::new();
    let mut recognized = false;
    for (field, role) in [("create", "owner"), ("manage", "admin"), ("join", "member")] {
        let Some(entries) = value.get(field) else {
            continue;
        };
        recognized = true;
        if entries.is_null() {
            continue;
        }
        let entries = entries.as_array().ok_or_else(schema_error)?;
        for entry in entries {
            let id = identifier(entry.get("gc"));
            if !is_numeric_id(&id) {
                return Err(schema_error());
            }
            if seen.insert(id.clone()) {
                groups.push(Group {
                    id,
                    name: decode_text(&text(entry.get("gn"))),
                    member_count: count(entry.get("member_count").or_else(|| entry.get("mem_num"))),
                    role: role.into(),
                });
            }
        }
    }
    if !recognized {
        return Err(schema_error());
    }
    Ok(groups)
}

fn parse_members(value: &Value) -> AppResult<FetchResult> {
    let entries = value
        .get("members")
        .and_then(Value::as_object)
        .ok_or_else(schema_error)?;
    let mut members = Vec::with_capacity(entries.len());
    for (qq_number, entry) in entries {
        if !entry.is_object() {
            return Err(schema_error());
        }
        members.push(Member {
            qq_number: qq_number.clone(),
            nickname: decode_text(&text(entry.get("nk"))),
            card: decode_text(&text(entry.get("cd"))),
            friend_relation: parse_friend_relation(entry.get("fr")),
        });
    }
    if members.is_empty() {
        return Err(AppError::new(
            "empty_members",
            "QQ 返回了空名单，无法判断是否有权限；请重新扫码登录并核对账号权限。",
        ));
    }
    let truncated = value
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("next_cursor")
            .is_some_and(|cursor| !text(Some(cursor)).is_empty());
    let warnings = if truncated {
        vec!["接口提示仍有未读取的数据，本次仅为部分名单。".into()]
    } else {
        Vec::new()
    };
    Ok(FetchResult {
        members,
        reported_count: count(value.get("mem_num")),
        freshness_confirmed: true,
        truncated,
        warnings,
    })
}

/// 好友接口使用独立的响应上限；HTML、JSON 数组或缺失结构不能伪装成“没有好友”。
///
/// 正文仅在内存中解析，错误和诊断事件不携带正文、昵称、分组名或好友号码。
fn parse_friend_list_bytes(bytes: &[u8]) -> AppResult<HashSet<String>> {
    if bytes.len() > MAX_FRIEND_LIST_BYTES {
        return Err(AppError::new(
            "response_too_large",
            "好友列表响应过大，未更新名单。请稍后重试。",
        ));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| friend_list_incomplete())?;
    parse_friend_list(&value)
}

/// 只提取已确认分组结构中的 `uin`，不采集昵称、备注、群组名和其他个人资料。
///
/// QQ 公开构建 7904f18b 的 index.2d8ec08f.js 一次请求后消费各分组的 mems，没有分页或独立总数。
/// 这里沿用该次观察到的一次性消费契约，不声称服务永远保证全量；出现分页信号立即拒绝。
/// 参考：https://qq-social-web.cdn-go.cn/qq-qun-web/7904f18b/assets/index.2d8ec08f.js
/// 空 `result` 无法区分无好友与异常，因此拒绝；非空分组的全空 `mems` 也必须有明确的 `total: 0`。
/// 当前观察未提供 `total`；若响应出现该字段，额外要求其与去重数一致，不把数组长度伪造为独立总数。
fn parse_friend_list(value: &Value) -> AppResult<HashSet<String>> {
    let object = value.as_object().ok_or_else(friend_list_incomplete)?;
    let ec = object.get("ec").ok_or_else(friend_list_incomplete)?;
    let errcode = object.get("errcode").ok_or_else(friend_list_incomplete)?;
    if !ec.is_i64() && !ec.is_u64() || !errcode.is_i64() && !errcode.is_u64() {
        return Err(friend_list_incomplete());
    }
    if ec.as_u64() != Some(0) {
        check_web_response(value)?;
        return Err(friend_list_incomplete());
    }
    if errcode.as_u64() != Some(0) {
        return Err(AppError::new(
            "qq_api_error",
            "QQ 暂时无法读取好友列表，未更新名单。请稍后重试。",
        ));
    }
    validate_friend_pagination(object)?;
    // 实际响应以分组键保存对象；公开页面使用 Object.values，也兼容数组，不依赖键名或分组资料。
    let groups = match object.get("result") {
        Some(Value::Object(groups)) if !groups.is_empty() => groups.values().collect::<Vec<_>>(),
        Some(Value::Array(groups)) if !groups.is_empty() => groups.iter().collect::<Vec<_>>(),
        _ => return Err(friend_list_incomplete()),
    };
    let mut qq_numbers = HashSet::new();
    for group in groups {
        let group = group.as_object().ok_or_else(friend_list_incomplete)?;
        validate_friend_pagination(group)?;
        let members = group
            .get("mems")
            .and_then(Value::as_array)
            .ok_or_else(friend_list_incomplete)?;
        for member in members {
            let raw = member
                .as_object()
                .and_then(|member| member.get("uin"))
                .ok_or_else(friend_list_incomplete)?;
            let number = identifier(Some(raw));
            if !is_numeric_id(&number) {
                return Err(friend_list_incomplete());
            }
            // 十进制字符串与整数代表同一号码；去前导零后跨分组去重，不把内部 UID 当 QQ 号。
            qq_numbers.insert(number.trim_start_matches('0').to_owned());
        }
    }
    let declared_total = match object.get("total") {
        Some(total) => Some(count(Some(total)).ok_or_else(friend_list_incomplete)?),
        None => None,
    };
    if declared_total.is_some_and(|total| total != qq_numbers.len())
        || qq_numbers.is_empty() && declared_total != Some(0)
    {
        return Err(friend_list_incomplete());
    }
    Ok(qq_numbers)
}

/// 当前观察的请求没有分页参数；新增分页字段属于协议变化，须先确认终止规则后才能放行。
///
/// 明确的 `has_more: false/0` 和空 cursor 可接受，其余游标、页码或未知真值均拒绝。
/// 该检查同时作用于响应和每个分组，避免某一组的部分数据被静默接受。
fn validate_friend_pagination(object: &serde_json::Map<String, Value>) -> AppResult<()> {
    for key in ["has_more", "hasMore", "more"] {
        if object
            .get(key)
            .is_some_and(|value| value != &Value::Bool(false) && value.as_u64() != Some(0))
        {
            return Err(friend_list_incomplete());
        }
    }
    for key in [
        "cursor",
        "next_cursor",
        "nextCursor",
        "next",
        "next_page",
        "nextPage",
    ] {
        if object
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
        {
            return Err(friend_list_incomplete());
        }
    }
    for key in [
        "page",
        "page_no",
        "pageNo",
        "page_index",
        "pageIndex",
        "page_count",
        "pageCount",
        "total_page",
        "totalPage",
        "total_pages",
        "totalPages",
    ] {
        if object.contains_key(key) {
            return Err(friend_list_incomplete());
        }
    }
    Ok(())
}

/// 不展示远端消息；失败后由宿主保留旧快照，用户可重试或明确关闭好友排除。
fn friend_list_incomplete() -> AppError {
    AppError::new(
        "friend_list_incomplete",
        "QQ 未返回可确认完整的好友列表，未更新名单。请重试，或取消“排除好友”后重新获取。",
    )
}

/// 应用只兼容明确的布尔值和整数 0/1，不把字符串真值或缺失字段猜成好友/非好友。
///
/// 群接口的 `fr` 仅保留兼容解析；本次启用好友排除时，宿主以实时好友列表覆盖原始目标全部成员的关系。
/// 好友列表失败不能回退到该字段；浮点数、字符串、其他整数及结构化值均保留 Unknown。
fn parse_friend_relation(value: Option<&Value>) -> crate::models::FriendRelation {
    use crate::models::FriendRelation;
    match value {
        Some(Value::Bool(true)) => FriendRelation::Friend,
        Some(Value::Bool(false)) => FriendRelation::NotFriend,
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
            match number.as_u64() {
                Some(1) => FriendRelation::Friend,
                Some(0) => FriendRelation::NotFriend,
                _ => FriendRelation::Unknown,
            }
        }
        _ => FriendRelation::Unknown,
    }
}

fn decode_text(value: &str) -> String {
    html_escape::decode_html_entities(value).replace('\u{a0}', " ")
}

fn session_account_id(session: &Session) -> AppResult<String> {
    let url = parse_fixed_url(MEMBER_PAGE)?;
    let raw = cookie(&session.jar, &url, "uin")
        .or_else(|| cookie(&session.jar, &url, "p_uin"))
        .unwrap_or_default();
    let number = raw.trim_start_matches('o').trim_start_matches('0');
    if !is_numeric_id(number) {
        return Err(login_required());
    }
    Ok(number.to_owned())
}

fn session_bkn(session: &Session) -> AppResult<u32> {
    let url = parse_fixed_url(MEMBER_PAGE)?;
    let raw = cookie(&session.jar, &url, "skey")
        .filter(|key| !key.is_empty())
        .or_else(|| cookie(&session.jar, &url, "p_skey"))
        .ok_or_else(login_required)?;
    Ok(hash33(&decode_cookie(&raw)?, 5381))
}

fn cookie(jar: &SessionCookies, url: &Url, name: &str) -> Option<String> {
    let header = jar.cookies(url)?;
    header.to_str().ok()?.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

/// Cookie 使用 URI 百分号解码，保留原始 +；表单解码会错误地把 + 转为空格。
fn decode_cookie(value: &str) -> AppResult<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            ) {
                output.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(output).map_err(|_| login_required())
}

/// QQ 的 GTK/bkn 与 ptqrtoken 共用 33 倍散列，分别从 5381 和 0 开始。
fn hash33(value: &str, seed: u32) -> u32 {
    value.chars().fold(seed, |hash, character| {
        hash.wrapping_mul(33).wrapping_add(character as u32)
    }) & 0x7fff_ffff
}

/// QQ 登录只允许 HTTPS、默认 HTTPS 端口、无用户信息的 qq.com 及其子域。
fn qq_https_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|host| host == "qq.com" || host.ends_with(".qq.com"))
}

fn parse_fixed_url(value: &str) -> AppResult<Url> {
    Url::parse(value)
        .map_err(|_| AppError::new("configuration_error", "内置接口地址无效，请重新安装程序。"))
}

fn login_required() -> AppError {
    AppError::new("login_required", "请先使用自己的 QQ 账号扫码登录。")
}

fn login_poll(status: &str, message: &str) -> LoginPoll {
    LoginPoll {
        status: status.into(),
        message: message.into(),
    }
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// 解析 ptuiCB 的字符串参数，支持逗号、引号与常见 JS 转义；从不执行远端脚本。
fn callback_arguments(source: &str) -> AppResult<Vec<String>> {
    let trimmed = source.trim().trim_end_matches(';').trim();
    let content = trimmed
        .strip_prefix("ptuiCB(")
        .and_then(|text| text.strip_suffix(')'))
        .ok_or_else(schema_error)?;
    let mut characters = content.chars().peekable();
    let mut arguments = Vec::new();
    loop {
        while characters.peek().is_some_and(|c| c.is_whitespace()) {
            characters.next();
        }
        if characters.peek().is_none() {
            break;
        }
        let quote = characters.next().ok_or_else(schema_error)?;
        if quote != '\'' && quote != '"' {
            return Err(schema_error());
        }
        let mut value = String::new();
        let mut closed = false;
        while let Some(character) = characters.next() {
            if character == quote {
                closed = true;
                break;
            }
            if character != '\\' {
                value.push(character);
                continue;
            }
            let escaped = characters.next().ok_or_else(schema_error)?;
            match escaped {
                '\\' | '\'' | '"' | '/' => value.push(escaped),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'x' | 'u' => {
                    let length = if escaped == 'x' { 2 } else { 4 };
                    let mut code = 0u32;
                    for _ in 0..length {
                        code = code * 16
                            + characters
                                .next()
                                .and_then(|c| c.to_digit(16))
                                .ok_or_else(schema_error)?;
                    }
                    if escaped == 'u' && (0xd800..=0xdbff).contains(&code) {
                        // JS 可将表情写成 UTF-16 代理对，不能把合法昵称误判成协议错误。
                        if characters.next() != Some('\\') || characters.next() != Some('u') {
                            return Err(schema_error());
                        }
                        let mut low = 0u32;
                        for _ in 0..4 {
                            low = low * 16
                                + characters
                                    .next()
                                    .and_then(|c| c.to_digit(16))
                                    .ok_or_else(schema_error)?;
                        }
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err(schema_error());
                        }
                        code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                    }
                    value.push(char::from_u32(code).ok_or_else(schema_error)?);
                }
                _ => return Err(schema_error()),
            }
        }
        if !closed {
            return Err(schema_error());
        }
        arguments.push(value);
        while characters.peek().is_some_and(|c| c.is_whitespace()) {
            characters.next();
        }
        match characters.next() {
            None => break,
            Some(',') => {
                while characters.peek().is_some_and(|c| c.is_whitespace()) {
                    characters.next();
                }
                if characters.peek().is_none() {
                    return Err(schema_error());
                }
            }
            _ => return Err(schema_error()),
        }
    }
    if arguments.len() < 5 {
        return Err(schema_error());
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 仅在内存装入合成 Cookie，用于请求构造、作用域和代次守卫测试，不访问 QQ 或磁盘。
    async fn memory_fixture() -> WebProvider {
        let jar = Arc::new(SessionCookies::default());
        let url = Url::parse(MEMBER_PAGE).unwrap();
        {
            let mut store = jar.0.write().unwrap();
            store
                .parse(
                    "uin=o001234567; Domain=qq.com; Path=/; Secure; HttpOnly",
                    &url,
                )
                .unwrap();
            store
                .parse(
                    "skey=fixture-only-session-secret; Path=/; Secure; HttpOnly",
                    &url,
                )
                .unwrap();
            store
                .parse(
                    "pt4_token=scoped-fixture; Path=/cgi-bin/qun_mgr; Secure; Max-Age=600",
                    &url,
                )
                .unwrap();
        }
        WebProvider {
            session: RwLock::new(Some(Arc::new(Session {
                generation: 1,
                client: session_client(jar.clone()).unwrap(),
                jar,
                started: Instant::now(),
                xlogin: url,
                authenticated: AtomicBool::new(true),
                nickname: RwLock::new("隔离测试".into()),
                poll_lock: Mutex::new(()),
                last_login_status: AtomicU8::new(4),
            }))),
            generation: AtomicU64::new(1),
        }
    }

    #[tokio::test]
    async fn session_is_memory_only_and_preserves_cookie_scope_and_cancellation() {
        let provider = memory_fixture().await;
        let session = provider.authenticated_session().await.unwrap();
        assert!(cookie(
            &session.jar,
            &Url::parse("https://other.qq.com/").unwrap(),
            "skey"
        )
        .is_none());
        assert!(cookie(&session.jar, &Url::parse(MEMBER_PAGE).unwrap(), "pt4_token").is_none());
        assert!(cookie(
            &session.jar,
            &Url::parse("https://qun.qq.com/cgi-bin/qun_mgr/get_group_list").unwrap(),
            "pt4_token"
        )
        .is_some());
        assert!(WebProvider::new()
            .unwrap()
            .authenticated_session()
            .await
            .is_err());
        provider.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            provider.authenticated_session().await.err().unwrap().code,
            "cancelled"
        );
    }

    #[test]
    fn remote_login_expiration_is_distinct_from_permission_or_business_rejection() {
        for value in [json!({"ec":4}), json!({"ec":"4"}), json!({"retcode":4})] {
            assert_eq!(
                check_web_response(&value).unwrap_err().code,
                "login_expired"
            );
        }
        assert_eq!(
            check_web_response(&json!({"ec":7})).unwrap_err().code,
            "group_forbidden"
        );
        assert_eq!(
            check_web_response(&json!({"ec":"unexpected-text"}))
                .unwrap_err()
                .code,
            "qq_api_error"
        );
    }

    #[test]
    fn callback_keeps_commas_and_escaped_quotes() {
        let arguments = callback_arguments(
            r#"ptuiCB('0','0','https://qun.qq.com/a?x=a,b','0','成功','a\'b, c');"#,
        )
        .unwrap();
        assert_eq!(arguments[2], "https://qun.qq.com/a?x=a,b");
        assert_eq!(arguments[5], "a'b, c");
        let emoji =
            callback_arguments(r#"ptuiCB('0','0','https://qun.qq.com','0','ok','\uD83D\uDE00');"#)
                .unwrap();
        assert_eq!(emoji[5], "😀");
        assert!(callback_arguments("alert('x')").is_err());
        assert!(callback_arguments("ptuiCB('0','0','x','0','unterminated)").is_err());
    }

    #[test]
    fn cookie_hash_preserves_plus_and_uses_distinct_seed() {
        assert_eq!(decode_cookie("a%2Bb+c").unwrap(), "a+b+c");
        assert_eq!(hash33("", 5381), 5381);
        assert_eq!(hash33("abc", 0), 108966);
        assert_eq!(hash33("abc", 5381), 193485963);
    }

    #[test]
    fn login_redirect_rejects_other_origins_and_credentials() {
        for target in [
            "http://qun.qq.com/",
            "https://qq.com.evil.test/",
            "https://qq.com@evil.test/",
            "https://evil@qq.com/",
            "https://qq.com:444/",
        ] {
            assert!(!qq_https_url(&Url::parse(target).unwrap()), "{target}");
        }
        assert!(qq_https_url(
            &Url::parse("https://ssl.ptlogin2.qq.com/path").unwrap()
        ));
    }

    #[test]
    fn missing_total_is_never_inferred_from_members() {
        let result = parse_members(&json!({"members":{"12345":{"nk":"A&amp;B","cd":""}}})).unwrap();
        assert_eq!(result.reported_count, None);
        assert_eq!(result.members[0].qq_number, "12345");
        assert_eq!(result.members[0].nickname, "A&B");
        assert_eq!(
            result.members[0].friend_relation,
            crate::models::FriendRelation::Unknown
        );
        assert!(parse_members(&json!({"members":[]})).is_err());
        assert!(parse_members(&json!({"members":{}})).is_err());
    }

    /// 明确兼容类型与异常类型逐项覆盖；尤其不能把字符串 "0" 按语言真值当成好友。
    #[test]
    fn friend_relation_accepts_only_boolean_or_integer_zero_one() {
        use crate::models::FriendRelation;
        for (value, expected) in [
            (json!(true), FriendRelation::Friend),
            (json!(false), FriendRelation::NotFriend),
            (json!(1), FriendRelation::Friend),
            (json!(0), FriendRelation::NotFriend),
            (json!(1.0), FriendRelation::Unknown),
            (json!(0.0), FriendRelation::Unknown),
            (json!(-1), FriendRelation::Unknown),
            (json!(2), FriendRelation::Unknown),
            (json!("0"), FriendRelation::Unknown),
            (json!("1"), FriendRelation::Unknown),
            (json!("false"), FriendRelation::Unknown),
            (json!(null), FriendRelation::Unknown),
            (json!([]), FriendRelation::Unknown),
            (json!({}), FriendRelation::Unknown),
        ] {
            assert_eq!(parse_friend_relation(Some(&value)), expected, "{value}");
            let parsed = parse_members(&json!({"members":{"100101":{"fr":value}}})).unwrap();
            assert_eq!(parsed.members[0].friend_relation, expected);
        }
        assert_eq!(parse_friend_relation(None), FriendRelation::Unknown);
    }

    /// 只合成结构和数量，不复制账号、昵称、分组名或任何用户附件中的真实好友数据。
    fn friend_list_fixture() -> Value {
        let mut sequence = 0u64;
        let mut groups = serde_json::Map::new();
        for index in 0..8 {
            let members = (0..if index == 7 { 66 } else { 67 })
                .map(|_| {
                    sequence += 1;
                    json!({"uin": 900_000_000_000_000_000u64 + sequence})
                })
                .collect::<Vec<_>>();
            // 默认分组可能只有 mems；不能把缺少 gid/gname 的有效分组误判为结构损坏。
            groups.insert((index + 1).to_string(), json!({"mems": members}));
        }
        json!({"ec": 0, "errcode": 0, "result": groups})
    }

    #[test]
    fn friends_parse_eight_synthetic_groups_and_deduplicate_decimal_ids() {
        let fixture = friend_list_fixture();
        let bytes = serde_json::to_vec(&fixture).unwrap();
        assert_eq!(parse_friend_list_bytes(&bytes).unwrap().len(), 535);
        let mixed = json!({"ec":0,"errcode":0,"result":[
            {"mems":[{"uin":900000000000000001u64}]},
            {"mems":[{"uin":"0900000000000000001"},{"uin":"900000000000000002"}]}
        ]});
        let parsed = parse_friend_list(&mixed).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains("900000000000000001"));
        assert!(parsed.contains("900000000000000002"));
    }

    #[test]
    fn friends_reject_missing_structure_statuses_invalid_ids_and_unproven_empty_result() {
        for invalid in [
            json!({}),
            json!({"ec":0,"result":[{"mems":[]}]}),
            json!({"ec":0,"errcode":null,"result":[{"mems":[]}]}),
            json!({"ec":"0","errcode":0,"result":[{"mems":[]}]}),
            json!({"ec":0.0,"errcode":0,"result":[{"mems":[]}]}),
            json!({"ec":0,"errcode":0,"result":null}),
            json!({"ec":0,"errcode":0,"result":[]}),
            json!({"ec":0,"errcode":0,"result":{}}),
            json!({"ec":0,"errcode":0,"result":[{}]}),
            json!({"ec":0,"errcode":0,"result":[{"mems":null}]}),
            json!({"ec":0,"errcode":0,"result":[{"mems":[]}]}),
            json!({"ec":0,"errcode":0,"result":[{"mems":[{}]}]}),
            json!({"ec":0,"errcode":0,"result":[{"mems":[null]}]}),
        ] {
            assert!(parse_friend_list(&invalid).is_err());
        }
        for invalid_uin in [
            json!(null),
            json!(false),
            json!(0),
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!(""),
            json!("0000"),
            json!(" 123"),
            json!("+123"),
            json!("1e5"),
            json!("123.0"),
            json!("１２３"),
            json!("internal_uid"),
            json!([]),
            json!({}),
            json!("123456789012345678901"),
        ] {
            let value = json!({"ec":0,"errcode":0,"result":[{"mems":[{"uin":invalid_uin}]}]});
            assert!(parse_friend_list(&value).is_err());
        }
        let mut failed = friend_list_fixture();
        failed["ec"] = json!(4);
        assert_eq!(
            parse_friend_list(&failed).unwrap_err().code,
            "login_expired"
        );
        failed["ec"] = json!(0);
        failed["errcode"] = json!(1);
        assert_eq!(parse_friend_list(&failed).unwrap_err().code, "qq_api_error");
        assert!(parse_friend_list_bytes(b"<html>login required</html>").is_err());
        assert!(parse_friend_list_bytes(b"[]").is_err());
        assert_eq!(
            parse_friend_list_bytes(&vec![b' '; MAX_FRIEND_LIST_BYTES + 1])
                .unwrap_err()
                .code,
            "response_too_large"
        );
    }

    #[test]
    fn friends_require_explicit_zero_for_empty_groups_and_consistent_optional_total() {
        let empty = json!({"ec":0,"errcode":0,"total":0,"result":{"1":{"mems":[]}}});
        assert!(parse_friend_list(&empty).unwrap().is_empty());
        let mut mismatch = friend_list_fixture();
        mismatch["total"] = json!(535);
        assert_eq!(parse_friend_list(&mismatch).unwrap().len(), 535);
        for total in [json!(0), json!(534), json!(536), json!(null), json!(535.0)] {
            mismatch["total"] = total;
            assert!(parse_friend_list(&mismatch).is_err());
        }
    }

    #[test]
    fn friends_refuse_pagination_markers_in_response_or_group() {
        for (field, marker) in [
            ("has_more", json!(true)),
            ("has_more", json!(1)),
            ("has_more", json!("0")),
            ("hasMore", json!(true)),
            ("next_cursor", json!("continuation")),
            ("cursor", json!(0)),
            ("next_page", json!(2)),
            ("page", json!(1)),
            ("total_pages", json!(1)),
            ("has_more", json!(null)),
        ] {
            let mut root = friend_list_fixture();
            root[field] = marker.clone();
            assert!(parse_friend_list(&root).is_err());
            let mut group = friend_list_fixture();
            group["result"]["1"][field] = marker;
            assert!(parse_friend_list(&group).is_err());
        }
        let mut terminal = friend_list_fixture();
        terminal["has_more"] = json!(false);
        terminal["next_cursor"] = json!("");
        terminal["result"]["1"]["has_more"] = json!(0);
        terminal["result"]["1"]["next_cursor"] = json!(null);
        assert_eq!(parse_friend_list(&terminal).unwrap().len(), 535);
    }

    /// 只构建请求而不发送；确认防跨域头、动态 bkn 和 POST 表单，不触碰真实 QQ 网络。
    #[tokio::test]
    async fn friends_request_uses_dynamic_bkn_post_and_fixed_origin() {
        let provider = memory_fixture().await;
        let session = provider.authenticated_session().await.unwrap();
        let first = friend_list_request(&session).unwrap().build().unwrap();
        let first_bkn = session_bkn(&session).unwrap().to_string();
        assert_eq!(first.method(), reqwest::Method::POST);
        assert_eq!(
            first.url().as_str().split('?').next().unwrap(),
            "https://qun.qq.com/cgi-bin/qun_mgr/get_friend_list"
        );
        assert_eq!(
            first.headers().get(reqwest::header::ORIGIN).unwrap(),
            "https://qun.qq.com"
        );
        assert_eq!(
            first.headers().get(reqwest::header::REFERER).unwrap(),
            MEMBER_PAGE
        );
        assert_eq!(
            first.body().unwrap().as_bytes().unwrap(),
            format!("bkn={first_bkn}").as_bytes()
        );
        let query = first
            .url()
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query.get("bkn").unwrap(), &first_bkn);
        assert!(query.get("ts").unwrap().parse::<u128>().unwrap() > 0);
        session
            .jar
            .0
            .write()
            .unwrap()
            .parse(
                "skey=second-synthetic-secret; Path=/; Secure; HttpOnly",
                &Url::parse(MEMBER_PAGE).unwrap(),
            )
            .unwrap();
        let second = friend_list_request(&session).unwrap().build().unwrap();
        let second_bkn = session_bkn(&session).unwrap().to_string();
        assert_ne!(second_bkn, first_bkn);
        assert!(second
            .url()
            .query_pairs()
            .any(|(key, value)| key == "bkn" && value == second_bkn));
        assert_eq!(
            second.body().unwrap().as_bytes().unwrap(),
            format!("bkn={second_bkn}").as_bytes()
        );
    }

    /// 把请求前/后的共同守卫直接回归；所有凭据均来自隔离测试夹具，不发出网络请求。
    #[tokio::test]
    async fn friends_reject_cancelled_superseded_or_changed_account() {
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            WebProvider::new()
                .unwrap()
                .friends(cancelled.clone())
                .await
                .err()
                .unwrap()
                .code,
            "cancelled"
        );
        let provider = memory_fixture().await;
        let session = provider.authenticated_session().await.unwrap();
        let expected = session_account_id(&session).unwrap();
        let active = CancellationToken::new();
        provider
            .ensure_friend_account(&session, &expected, &active)
            .unwrap();
        assert_eq!(
            provider
                .ensure_friend_account(&session, &expected, &cancelled)
                .unwrap_err()
                .code,
            "cancelled"
        );
        session
            .jar
            .0
            .write()
            .unwrap()
            .parse(
                "uin=o009999999; Domain=qq.com; Path=/; Secure; HttpOnly",
                &Url::parse(MEMBER_PAGE).unwrap(),
            )
            .unwrap();
        assert_eq!(
            provider
                .ensure_friend_account(&session, &expected, &active)
                .unwrap_err()
                .code,
            "account_changed"
        );
        provider.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            provider
                .ensure_friend_account(&session, &expected, &active)
                .unwrap_err()
                .code,
            "cancelled"
        );
    }

    /// 本地一次性 HTTP 夹具只发送固定文本，验证状态码与响应长度边界，不带任何 Cookie。
    #[tokio::test]
    async fn friends_http_status_and_body_limit_reject_before_json_parsing() {
        use std::io::{Read, Write};
        for (status, length, expected) in [
            (401, 0, "unauthorized"),
            (429, 0, "rate_limited"),
            (200, MAX_FRIEND_LIST_BYTES + 1, "response_too_large"),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
            });
            let client = Client::builder().no_proxy().build().unwrap();
            let result = request_bytes(
                "friends",
                client.get(format!("http://{address}/synthetic")),
                MAX_FRIEND_LIST_BYTES,
                false,
            )
            .await;
            server.join().unwrap();
            assert_eq!(result.unwrap_err().code, expected);
        }
    }

    #[test]
    fn groups_include_joined_groups_and_preserve_strongest_role() {
        let groups = parse_groups(&json!({"create":[{"gc":12345,"gn":"A"}],"manage":[],"join":[{"gc":12345,"gn":"A"},{"gc":"23456","gn":"B"}]})).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].role, "owner");
        assert_eq!(groups[1].role, "member");
        assert_eq!(groups[1].member_count, None);
        assert!(parse_groups(&json!({"ec":0})).is_err());
    }
}
