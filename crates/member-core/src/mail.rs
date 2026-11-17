//! TLS SMTP 邮件构建、逐封投递与单封密送。队列、取消、发送确认和授权由桌面宿主管理。
//! 本模块只在内存中使用密码、附件和正文；宿主可按明确保存操作加密保管授权码。
//! SMTP 故障只记录脱敏后的分类、响应码及错误链，不记录协议原文或邮件内容，不自动重试不确定的投递。
//! 作者：ahui。

use crate::{
    diagnostics,
    models::{AppError, AppResult},
};
use ammonia::{Builder, UrlRelative};
use lettre::{
    message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart},
    transport::smtp::{authentication::Credentials, Error as SmtpError},
    Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    net::IpAddr,
    sync::OnceLock,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// 保守的原始附件合计上限；MIME 编码会进一步增大邮件，服务商仍可能有更低限制。
pub const MAX_ATTACHMENT_BYTES: usize = 18 * 1024 * 1024;
/// 单张正文图片原始字节上限；正文图片和普通附件另外共用 18 MiB 的发送上限。
pub const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// 一封邮件最多十张正文图片，重复引用同一张只占一个 MIME 部件。
pub const MAX_INLINE_IMAGES: usize = 10;
/// 限制正文输入，避免清理 HTML 与编码时出现不必要的内存开销。
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// 单次导入收件人技术保护上限，不代表邮箱服务商允许的发送人数或发送频率。
pub const MAX_RECIPIENTS: usize = 10_000;
/// 单封密送的程序保护上限；服务商的实际 RCPT 上限可能更低，不能据此承诺可一次发送千人。
pub const MAX_BATCH_RECIPIENTS: usize = 1_000;

/// 将已完成全局去重、历史过滤和未决检查的收件范围按已确认账号顺序连续均分。
///
/// 返回每个选中账号在原始待发数组中的半开区间，前 `N % M` 个账号多承担一人；
/// 如需随机均分，调用方应在预检先随机并冻结账号与待发数组的顺序，本函数不重复打乱。
/// 零人时仍为每个账号返回空区间；账号数必须大于零，不能退回隐式单账号。
/// 此函数不访问账号凭据或投递记录，调用方须先完成全局过滤，再在各区间内分批，
/// 不得把失败批次重新分配给其他账号。时间、空间复杂度均为 O(M)。
pub fn balanced_recipient_ranges(
    recipient_count: usize,
    account_count: usize,
) -> AppResult<Vec<std::ops::Range<usize>>> {
    if account_count == 0 {
        return Err(AppError::new(
            "mail_accounts_empty",
            "请至少选择一个发件账号。",
        ));
    }
    let quotient = recipient_count / account_count;
    let remainder = recipient_count % account_count;
    let mut start = 0;
    Ok((0..account_count)
        .map(|index| {
            let count = quotient + usize::from(index < remainder);
            let range = start..start + count;
            start += count;
            range
        })
        .collect())
}

/// 仅供后端使用的 SMTP 配置。密码不实现 Debug 或 Serialize，避免意外回显。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSettings {
    /// SMTP 域名或 IP，不包含协议、路径、端口或用户信息。
    pub host: String,
    /// 服务商指定的非零端口，通常隐式 TLS 为 465、STARTTLS 为 587。
    pub port: u16,
    /// tls 表示建立连接即 TLS；starttls 表示必须升级为 TLS，禁止明文降级。
    pub tls_mode: String,
    /// SMTP 认证账号，不根据发件地址自动猜测。
    pub username: String,
    /// SMTP 密码或邮箱授权码，原样传入认证；仅独立凭据仓库可加密保存，不写 SQLite 或日志。
    pub password: String,
    /// 实际 From 邮箱，必须为合法裸邮箱地址。
    pub from_address: String,
    /// 可选显示姓名；禁止换行或控制字符。
    pub from_name: String,
}

/// 已校验的收件地址；只有数字@qq.com 地址能够可靠映射到 QQ 号。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailRecipient {
    /// 通过 lettre 地址解析并将域名转为小写的完整收件邮箱。
    pub email: String,
    /// 仅数字@qq.com 时填入 QQ 号；其他域名或 QQ 别名邮箱均为 None。
    pub qq_number: Option<String>,
}

/// 用户主动选择的文件或正文图片。内容不写日志，所有文件只在内存中构建 MIME。
#[derive(Clone)]
pub struct MailAttachment {
    /// 仅用于 MIME 显示，构建邮件时会移除路径、控制字符和危险文件名字符。
    pub name: String,
    /// 用户主动选择的文件内容，所有附件合计不能超过 MAX_ATTACHMENT_BYTES。
    pub bytes: Vec<u8>,
    /// None 为普通附件；Some 只允许已校验的本机栅格图片，不能引用外部网络资源。
    pub inline: Option<InlineImageMetadata>,
}

/// 正文图片的 MIME 标识；CID 由宿主生成，不能包含路径、换行或任意 HTML。
#[derive(Clone)]
pub struct InlineImageMetadata {
    /// 不带尖括号的 Content-ID，发送 HTML 通过 cid: 引用，与邮箱收件人无关。
    pub content_id: String,
    /// 真实图片格式对应的 MIME，必须与文件头一致，不能由文件扩展名代替校验。
    pub mime_type: String,
}

/// 校验图片真实格式及尺寸，只读取头部，不下载 URL 或解码超大像素缓冲区。
///
/// 输入须为非空 PNG/JPEG/GIF/WebP，最多 5 MiB、每边不超过 8192 像素、总像素不超过
/// 4000 万。声明 MIME 必须与文件格式一致；SVG 和任意 HTML 不可作为正文图片。
pub fn validate_inline_image(bytes: &[u8], mime_type: &str) -> AppResult<()> {
    if inline_image_mime_type(bytes)? != mime_type {
        return Err(AppError::new(
            "inline_image_format",
            "图片类型与实际内容不同，请重新复制或选择图片。",
        ));
    }
    Ok(())
}

/// 按文件内容识别允许的正文图片，校验原始大小与头部尺寸，返回标准 MIME。
///
/// 本机选择器没有可信 MIME，可调用本函数；不依赖扩展名、不解码整张像素图，
/// 以限制预览和 MIME 构建前的内存开销。格式、大小或尺寸不合法时返回可读错误。
pub fn inline_image_mime_type(bytes: &[u8]) -> AppResult<&'static str> {
    if bytes.is_empty() || bytes.len() > MAX_INLINE_IMAGE_BYTES {
        return Err(AppError::new(
            "inline_image_size",
            "每张正文图片须大于 0 字节且不超过 5 MiB。",
        ));
    }
    let format = image::guess_format(bytes).map_err(|_| {
        AppError::new(
            "inline_image_format",
            "无法识别图片，请使用 PNG、JPEG、GIF 或 WebP。",
        )
    })?;
    let expected = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => {
            return Err(AppError::new(
                "inline_image_format",
                "正文仅支持 PNG、JPEG、GIF 或 WebP 图片。",
            ))
        }
    };
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| {
            AppError::new(
                "inline_image_format",
                "图片内容损坏或无法读取，请重新复制图片。",
            )
        })?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 40_000_000
    {
        return Err(AppError::new(
            "inline_image_dimensions",
            "图片尺寸过大，请缩小到每边 8192 像素以内、总像素不超过 4000 万。",
        ));
    }
    Ok(expected)
}

/// 解析换行、逗号、分号分隔的裸邮箱地址；任何无效项使整个导入失败，避免静默遗漏。
///
/// 域名按小写归一化后去重，保留本地部分大小写以兼容区分大小写的邮件系统。
/// 空白输入返回空列表，超过 MAX_RECIPIENTS 时拒绝；不会从普通邮箱猜测 QQ 号。
pub fn parse_recipients(input: &str) -> AppResult<Vec<MailRecipient>> {
    if input.len() > 512 * 1024 {
        return Err(AppError::new(
            "recipients_too_large",
            "收件人文本过大，请分批导入。",
        ));
    }
    let mut recipients = Vec::new();
    let mut seen = HashSet::new();
    for (index, raw) in input
        .split(['\n', '\r', ',', ';', '，', '；'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        let address: Address = raw.parse().map_err(|_| {
            AppError::new(
                "invalid_recipient",
                format!("第 {} 个收件地址格式无效，请填写完整邮箱地址。", index + 1),
            )
        })?;
        let domain = address.domain().to_ascii_lowercase();
        let email = format!("{}@{}", address.user(), domain);
        if !seen.insert(email.clone()) {
            continue;
        }
        if recipients.len() >= MAX_RECIPIENTS {
            return Err(AppError::new(
                "too_many_recipients",
                format!("一次最多导入 {MAX_RECIPIENTS} 个收件地址，请分批处理。"),
            ));
        }
        let qq_number =
            (domain == "qq.com" && numeric_qq(address.user())).then(|| address.user().to_owned());
        recipients.push(MailRecipient { email, qq_number });
    }
    Ok(recipients)
}

/// 清理邮件正文，仅保留基础排版和安全链接。
///
/// 删除脚本、事件属性、远程图片、内嵌表单与任意样式；链接仅允许 http/https/mailto。
/// 使用白名单清理而非直接发送编辑器 HTML，保证预览之外的输入也经过同样边界。
pub fn sanitize_body(html: &str) -> String {
    sanitize_body_with_images(html, &HashSet::new())
}

/// 只让已注册 CID 出现在 img.src；发送与模板保存/预览共同使用同一清理边界。
///
/// `image_ids` 只接受宿主或模板仓库校验过的 Content-ID；不能直接使用正文中的地址构造白名单。
/// 本函数不读取文件、不联网；调用方仍须在进入清理器前限制正文与资源大小。
pub fn sanitize_body_with_images(html: &str, image_ids: &HashSet<String>) -> String {
    let mapping = image_ids
        .iter()
        .map(|id| (id.clone(), id.clone()))
        .collect();
    sanitize_body_with_image_mapping(html, &mapping)
}

/// 清理正文并仅在真实 img.src 属性中把旧 CID 映射为新 CID，供应用模板时重新登记资源。
///
/// `image_ids` 的键、值都必须是宿主已校验的 Content-ID；空表移除全部图片，未登记引用不保留。
/// 映射以解析后的属性为单位同时完成，不串联替换，正文、代码示例及其他属性中的相同文字保持原样。
/// 本函数不读取文件或联网；调用方仍须先限制正文/资源大小并核实图片格式与 CID 合法性。
pub fn sanitize_body_with_image_mapping(html: &str, image_ids: &HashMap<String, String>) -> String {
    let tags: HashSet<&str> = [
        "a",
        "b",
        "strong",
        "i",
        "em",
        "u",
        "s",
        "del",
        "p",
        "br",
        "div",
        "span",
        "ul",
        "ol",
        "li",
        "blockquote",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "pre",
        "code",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "th",
        "td",
    ]
    .into_iter()
    .collect();
    let remove_content: HashSet<&str> = [
        "script", "style", "iframe", "object", "embed", "svg", "math", "form", "textarea", "select",
    ]
    .into_iter()
    .collect();
    let attributes: HashMap<&str, HashSet<&str>> = [
        ("a", ["href", "title"].into_iter().collect()),
        ("ol", ["start"].into_iter().collect()),
        ("td", ["colspan", "rowspan"].into_iter().collect()),
        ("th", ["colspan", "rowspan"].into_iter().collect()),
    ]
    .into_iter()
    .collect();
    let mut builder = Builder::default();
    builder
        .tags(tags)
        .clean_content_tags(remove_content)
        .generic_attributes(HashSet::new())
        .tag_attributes(attributes)
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true);
    if !image_ids.is_empty() {
        let allowed = image_ids.clone();
        builder
            .add_tags(["img"].iter())
            .add_tag_attributes("img", ["src", "alt", "width", "height"].iter())
            .add_url_schemes(["cid"].iter())
            .attribute_filter(move |tag, attribute, value| {
                if tag == "img" && attribute == "src" {
                    return value
                        .strip_prefix("cid:")
                        .and_then(|id| allowed.get(id))
                        .map(|id| std::borrow::Cow::Owned(format!("cid:{id}")));
                }
                if tag == "a" && attribute == "href" && value.starts_with("cid:") {
                    return None;
                }
                Some(std::borrow::Cow::Owned(value.to_owned()))
            });
    }
    builder.clean(html).to_string()
}

/// 已配置的安全 SMTP 发送器，构建时不联网。认证凭据由 lettre 保存在进程内存中。
pub struct MailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    /// 已校验的连接参数用于诊断；不包含账号或邮箱身份，不实现 Debug/Serialize。
    host: String,
    port: u16,
    tls_mode: String,
    /// 服务商可能在错误中回显身份；保存最小脱敏字典，发送器释放时擦除额外明文副本。
    diagnostic_secrets: Zeroizing<Vec<String>>,
}

/// 宿主提供的批次关联范围；仅用于日志，不改变发送内容、连接参数或投递状态。
/// job_id 必须使用宿主生成的随机任务标识，batch_index 从 1 开始，禁止传入收件人或主题。
#[derive(Clone, Copy)]
pub struct MailSendContext<'a> {
    /// 本进程内宿主创建的随机任务号；不允许使用用户输入或真实邮箱替代。
    pub job_id: &'a str,
    /// 日志展示使用的批次序号，从 1 开始，不能用于改变程序的分批顺序。
    pub batch_index: usize,
}

/// 仅抽取错误诊断，绝不据此判为已送达或自动重试；SMTP 4xx/5xx 也保留原有 unknown 业务语义。
/// secrets 必须包含当前认证身份和收件范围；错误链最多 8 层，避免异常 source 链无限循环。
fn smtp_error_details(error: &SmtpError, secrets: &[&str]) -> Value {
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_tls() {
        "tls"
    } else if error.is_transient() {
        "smtp_transient"
    } else if error.is_permanent() {
        "smtp_permanent"
    } else if error.is_response() {
        "response_parse"
    } else if error.is_client() {
        "client"
    } else if error.is_transport_shutdown() {
        "transport_shutdown"
    } else {
        "connection_or_network"
    };
    let mut chain = vec![diagnostics::sanitize(&error.to_string(), secrets)];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if chain.len() >= 8 {
            break;
        }
        chain.push(diagnostics::sanitize(&cause.to_string(), secrets));
        source = cause.source();
    }
    static EXTENDED_STATUS: OnceLock<Regex> = OnceLock::new();
    let pattern = EXTENDED_STATUS.get_or_init(|| {
        Regex::new(r"\b[245]\.[0-9]{1,3}\.[0-9]{1,3}\b")
            .expect("constant SMTP extended status regex")
    });
    let mut extended = Vec::new();
    for line in &chain {
        for code in pattern.find_iter(line) {
            if !extended.iter().any(|value| value == code.as_str()) {
                extended.push(code.as_str().to_owned());
            }
        }
    }
    json!({
        "category": category,
        "smtp_status": error.status().map(|code| code.to_string()),
        "extended_status": extended,
        "error_chain": chain,
    })
}

/// 只把已确认的 550 + Too many attempts 响应译为固定频率提示，不把其他 550、认证或网络错误混为一谈。
/// 自由响应仅在内存中做匹配，绝不回显给页面；错误码、unknown 判定、重试与批次继续策略保持原样。
fn delivery_error_message(error: &SmtpError, batch: bool) -> &'static str {
    let is_550 = error.status().is_some_and(|code| code.to_string() == "550");
    let frequency_limited = if is_550 {
        static TOO_MANY_ATTEMPTS: OnceLock<Regex> = OnceLock::new();
        let pattern = TOO_MANY_ATTEMPTS.get_or_init(|| {
            Regex::new(r"(?i)\btoo\s+many\s+attempts\b")
                .expect("constant SMTP frequency response regex")
        });
        // lettre 将 SMTP 负响应正文保存在 source 中；只有状态码与明确原因同时匹配才采用此文案。
        std::error::Error::source(error).is_some_and(|source| pattern.is_match(&source.to_string()))
    } else {
        false
    };
    if frequency_limited {
        if batch {
            "发送过于频繁，请稍后再试。请先核实本批投递记录，避免重复发送。"
        } else {
            "发送过于频繁，请稍后再试。请先核实本封投递记录，避免重复发送。"
        }
    } else if batch {
        "未能确认本批邮件的投递结果，请先核实邮箱或服务商记录；请勿直接拆分重发。"
    } else {
        "未能确认本封邮件的投递结果，请先核实邮箱或服务商记录，再决定是否重发。"
    }
}

impl MailSender {
    /// 校验配置并建立传输对象。TLS 使用系统信任证书，禁止忽略证书或明文回退。
    ///
    /// 不发起 SMTP 连接；用户名、密码、发件地址缺失或 TLS 模式无效时返回可读错误。
    pub fn new(settings: &MailSettings) -> AppResult<Self> {
        validate_settings(settings)?;
        let address: Address =
            settings.from_address.trim().parse().map_err(|_| {
                AppError::new("invalid_from", "发件邮箱格式无效，请填写完整邮箱地址。")
            })?;
        let name =
            (!settings.from_name.trim().is_empty()).then(|| settings.from_name.trim().to_owned());
        let from = Mailbox::new(name, address);
        let builder = match settings.tls_mode.as_str() {
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(settings.host.trim()),
            "starttls" => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(settings.host.trim())
            }
            _ => unreachable!("TLS mode is validated before transport construction"),
        }
        .map_err(|_| {
            AppError::new(
                "invalid_smtp_host",
                "无法为 SMTP 地址建立安全连接配置，请核对服务器名称。",
            )
        })?;
        let transport = builder
            .port(settings.port)
            .credentials(Credentials::new(
                settings.username.clone(),
                settings.password.clone(),
            ))
            .timeout(Some(Duration::from_secs(60)))
            .build();
        let sender = Self {
            transport,
            from,
            host: settings.host.trim().to_owned(),
            port: settings.port,
            tls_mode: settings.tls_mode.clone(),
            diagnostic_secrets: Zeroizing::new(vec![
                settings.password.clone(),
                settings.username.clone(),
                settings.from_address.clone(),
                settings.from_name.clone(),
            ]),
        };
        sender.log_transport("info", "configured", None, 0, None, None, None);
        Ok(sender)
    }

    /// 测试 TLS 连接与 SMTP 认证，不发送邮件；仅应由用户主动点击连接检查时调用。
    pub async fn test_connection(&self) -> AppResult<()> {
        let started = Instant::now();
        self.log_transport("info", "connection_test_started", None, 0, None, None, None);
        match self.transport.test_connection().await {
            Ok(true) => {
                self.log_transport(
                    "info",
                    "connection_test_finished",
                    None,
                    0,
                    Some(started),
                    Some("connected"),
                    None,
                );
                Ok(())
            }
            result => {
                // lettre 的 NOOP 检查返回 false 时已隐藏底层原因；明确记录这个边界，不虚构 SMTP 状态码。
                let details = Some(match result {
                    Err(error) => self.error_details(&error, &[]),
                    Ok(false) => {
                        json!({"category": "connection_check_negative", "smtp_status": null})
                    }
                    Ok(true) => unreachable!("successful connection handled above"),
                });
                self.log_transport(
                    "error",
                    "connection_test_finished",
                    None,
                    0,
                    Some(started),
                    Some("failed"),
                    details,
                );
                Err(AppError::new(
                    "smtp_connection_failed",
                    "SMTP 安全连接或认证失败，请核对服务器、端口、TLS 模式及邮箱授权码。",
                ))
            }
        }
    }

    /// 统一诊断字段保证宿主与底层按同一 job/batch 串联；只写人数，不写实际收件地址。
    /// 各字段集中在同一入口，避免不同阶段重复拼装连接信息而遗漏脱敏；参数不参与业务判断。
    #[allow(clippy::too_many_arguments)]
    fn log_transport(
        &self,
        level: &str,
        event: &str,
        context: Option<MailSendContext<'_>>,
        recipient_count: usize,
        started: Option<Instant>,
        outcome: Option<&str>,
        details: Option<Value>,
    ) {
        diagnostics::event(
            level,
            "smtp",
            event,
            json!({
                "job_id": context.map(|value| value.job_id),
                "batch_index": context.map(|value| value.batch_index),
                "host": diagnostics::sanitize(&self.host, &self.diagnostic_secrets.iter().map(String::as_str).collect::<Vec<_>>()),
                "port": self.port,
                "tls_mode": self.tls_mode,
                "timeout_seconds": 60,
                "recipient_count": recipient_count,
                "elapsed_ms": started.map(|value| value.elapsed().as_millis()),
                "outcome": outcome,
                "details": details,
            }),
        );
    }

    /// 认证值与当前整批地址都参与脱敏；字典只在本次同步日志构造期间借用，不进入日志字段。
    fn error_details(&self, error: &SmtpError, recipients: &[String]) -> Value {
        let secrets = self
            .diagnostic_secrets
            .iter()
            .chain(recipients)
            .map(String::as_str)
            .collect::<Vec<_>>();
        smtp_error_details(error, &secrets)
    }

    /// 在建立投递记录或连接 SMTP 前，完整检查本地草稿能否安全构建。
    ///
    /// `recipient` 是已由收件人解析器校验的任一候选地址；主题、正文与附件在整批中共用。
    /// 此方法只构建并丢弃内存中的邮件，不测试服务器、不发邮件，错误不会产生“投递待核实”记录。
    pub fn validate_draft(
        &self,
        recipient: &str,
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
    ) -> AppResult<()> {
        self.build_message(recipient, subject, html, attachments)
            .map(|_| ())
    }

    /// 构建并投递一封单收件人邮件，不使用 CC/BCC，也不自动重试。
    ///
    /// 成功表示 SMTP 服务器接受邮件，不保证收件箱最终到达；错误可能发生在服务器接受之后，
    /// 调用方必须保留“投递未确认”状态，避免自动重试导致重复发送。发送中的取消由宿主停止后续邮件。
    pub async fn send(
        &self,
        recipient: &str,
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
    ) -> AppResult<()> {
        self.send_with_context(recipient, subject, html, attachments, None)
            .await
    }

    /// 与 send 的投递语义相同；context 仅给已存在的宿主任务附加日志关联信息。
    pub async fn send_with_context(
        &self,
        recipient: &str,
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
        context: Option<MailSendContext<'_>>,
    ) -> AppResult<()> {
        let started = Instant::now();
        let message = self
            .build_message(recipient, subject, html, attachments)
            .inspect_err(|error| {
                self.log_transport(
                    "error",
                    "draft_build_failed",
                    context,
                    1,
                    Some(started),
                    Some(&error.code),
                    None,
                );
            })?;
        self.send_message(message, &[recipient.to_owned()], false, context, started)
            .await
    }

    /// 一封邮件同时投递给多位收件人，地址进入 SMTP envelope 的 BCC，邮件正文头部不暴露名单。
    ///
    /// 只发送一个 Message，不逐人重发；空列表或超过程序单封上限时直接拒绝。
    /// 若 SMTP 服务商有更低人数上限则可能失败，调用方必须停止该批次且不能自动拆小重试，
    /// 因为连接失败发生前服务器可能已经接收了部分收件人或整封邮件。
    pub async fn send_batch(
        &self,
        recipients: &[String],
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
    ) -> AppResult<()> {
        self.send_batch_with_context(recipients, subject, html, attachments, None)
            .await
    }

    /// 与 send_batch 的投递语义相同；不会因为有诊断上下文而拆分、重试或改判失败批次。
    pub async fn send_batch_with_context(
        &self,
        recipients: &[String],
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
        context: Option<MailSendContext<'_>>,
    ) -> AppResult<()> {
        let started = Instant::now();
        let message = self
            .build_batch_message(recipients, subject, html, attachments)
            .inspect_err(|error| {
                self.log_transport(
                    "error",
                    "draft_build_failed",
                    context,
                    recipients.len(),
                    Some(started),
                    Some(&error.code),
                    None,
                );
            })?;
        self.send_message(message, recipients, true, context, started)
            .await
    }

    /// 只包围现有单次 transport.send；成功响应不记录服务端正文，失败详情脱敏后写日志。
    /// 对外仍返回原有统一错误，未知投递的阻止和人工核实规则完全交由既有宿主逻辑处理。
    async fn send_message(
        &self,
        message: Message,
        recipients: &[String],
        batch: bool,
        context: Option<MailSendContext<'_>>,
        started: Instant,
    ) -> AppResult<()> {
        self.log_transport(
            "info",
            if batch {
                "batch_send_started"
            } else {
                "send_started"
            },
            context,
            recipients.len(),
            None,
            None,
            None,
        );
        match self.transport.send(message).await {
            Ok(response) => {
                self.log_transport(
                    "info",
                    "send_finished",
                    context,
                    recipients.len(),
                    Some(started),
                    Some("smtp_accepted"),
                    Some(json!({"smtp_status": response.code().to_string()})),
                );
                Ok(())
            }
            Err(error) => {
                self.log_transport(
                    "error",
                    "send_finished",
                    context,
                    recipients.len(),
                    Some(started),
                    Some("delivery_unknown"),
                    Some(self.error_details(&error, recipients)),
                );
                Err(AppError::new(
                    "mail_delivery_unknown",
                    delivery_error_message(&error, batch),
                ))
            }
        }
    }

    /// 生成 UTF-8 纯文本/HTML 双版本和附件；独立构建便于测试，不触碰外部邮箱。
    fn build_message(
        &self,
        recipient: &str,
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
    ) -> AppResult<Message> {
        self.build_addressed_message(&[recipient.to_owned()], false, subject, html, attachments)
    }

    /// 为测试和批量发送统一构建密送邮件，所有地址只进入 envelope，不保留 Bcc 头。
    fn build_batch_message(
        &self,
        recipients: &[String],
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
    ) -> AppResult<Message> {
        self.build_addressed_message(recipients, true, subject, html, attachments)
    }

    /// 共同构建路径确保两种发送模式使用完全相同的正文清理和附件限制。
    fn build_addressed_message(
        &self,
        recipients: &[String],
        blind_copy: bool,
        subject: &str,
        html: &str,
        attachments: &[MailAttachment],
    ) -> AppResult<Message> {
        if recipients.is_empty() || recipients.len() > MAX_BATCH_RECIPIENTS {
            return Err(AppError::new(
                "invalid_batch_size",
                format!(
                    "单封邮件请指定 1—{MAX_BATCH_RECIPIENTS} 位收件人；邮箱服务商限制可能更低。"
                ),
            ));
        }
        let mut addresses = Vec::new();
        let mut seen = HashSet::new();
        for recipient in recipients {
            let address: Address = recipient.trim().parse().map_err(|_| {
                AppError::new("invalid_recipient", "收件邮箱格式无效，请重新检查收件人。")
            })?;
            let canonical = format!(
                "{}@{}",
                address.user(),
                address.domain().to_ascii_lowercase()
            );
            if seen.insert(canonical) {
                addresses.push(address);
            }
        }
        if subject.trim().is_empty() || subject.len() > 998 || subject.chars().any(char::is_control)
        {
            return Err(AppError::new(
                "invalid_subject",
                "请填写有效邮件主题，不要包含换行或控制字符，且不要过长。",
            ));
        }
        if html.len() > MAX_BODY_BYTES {
            return Err(AppError::new(
                "body_too_large",
                "邮件正文超过 2 MiB，请精简正文后再发送。",
            ));
        }
        let total_bytes = attachments.iter().try_fold(0usize, |total, attachment| {
            total.checked_add(attachment.bytes.len())
        });
        if total_bytes.is_none_or(|total| total > MAX_ATTACHMENT_BYTES) {
            return Err(AppError::new(
                "attachments_too_large",
                "正文图片与附件合计超过 18 MiB，请压缩或分开发送。",
            ));
        }
        let image_count = attachments
            .iter()
            .filter(|file| file.inline.is_some())
            .count();
        if image_count > MAX_INLINE_IMAGES || attachments.len() - image_count > 10 {
            return Err(AppError::new(
                "too_many_attachments",
                "单封邮件最多添加 10 个普通附件和 10 张正文图片。",
            ));
        }

        // 图片由宿主登记后带入内存；两种发送方式都再次验证，不能把 HTML 中的 URL
        // 当作可下载资源，也不能把用户输入直接用作 MIME 头部的 Content-ID。
        let mut image_ids = HashSet::new();
        for attachment in attachments {
            if let Some(image) = &attachment.inline {
                if image.content_id.is_empty()
                    || image.content_id.len() > 128
                    || !image
                        .content_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"@._-".contains(&byte))
                    || !image_ids.insert(image.content_id.clone())
                {
                    return Err(AppError::new(
                        "inline_image_id",
                        "正文图片标识无效或重复，请移除后重新插入图片。",
                    ));
                }
                validate_inline_image(&attachment.bytes, &image.mime_type)?;
            }
        }
        let html = sanitize_body_with_images(html, &image_ids);
        let plain = plain_alternative(&html);
        let alternatives = if image_ids.is_empty() {
            MultiPart::alternative_plain_html(plain, html)
        } else {
            // related 只包含 HTML 及其 CID 图片；普通附件属于最外层 mixed。
            // 这样邮箱客户端能在正文显示图片，同时仍有不含 HTML 的纯文本版本。
            let mut related = MultiPart::related().singlepart(SinglePart::html(html));
            for attachment in attachments {
                if let Some(image) = &attachment.inline {
                    let mime = ContentType::parse(&image.mime_type).map_err(|_| {
                        AppError::new("inline_image_format", "无法生成正文图片类型。")
                    })?;
                    related = related.singlepart(
                        Attachment::new_inline_with_name(
                            image.content_id.clone(),
                            attachment_name(&attachment.name),
                        )
                        .body(attachment.bytes.clone(), mime),
                    );
                }
            }
            MultiPart::alternative()
                .singlepart(SinglePart::plain(plain))
                .multipart(related)
        };
        let mut body = MultiPart::mixed().multipart(alternatives);
        for attachment in attachments.iter().filter(|file| file.inline.is_none()) {
            let name = attachment_name(&attachment.name);
            // 统一使用二进制附件类型，不从扩展名推断可执行或可嵌入的 HTML 内容。
            let mime = ContentType::parse("application/octet-stream").map_err(|_| {
                AppError::new("mail_build_error", "无法生成附件类型，请重新启动程序。")
            })?;
            body = body.singlepart(Attachment::new(name).body(attachment.bytes.clone(), mime));
        }
        let mut message = Message::builder().from(self.from.clone()).subject(subject);
        for address in addresses {
            message = if blind_copy {
                // lettre 先从 Bcc 生成 envelope，再默认删除序列化邮件的 Bcc 头；不能调用 keep_bcc。
                message.bcc(Mailbox::new(None, address))
            } else {
                message.to(Mailbox::new(None, address))
            };
        }
        message.multipart(body).map_err(|_| {
            AppError::new(
                "mail_build_error",
                "无法生成邮件，请检查发件人、主题和附件名称。",
            )
        })
    }
}

/// 服务地址使用裸域名或 IP，不接收 URL，从源头避免错把用户名/端口拼进连接地址。
fn validate_settings(settings: &MailSettings) -> AppResult<()> {
    let host = settings.host.trim();
    let valid_host = host.parse::<IpAddr>().is_ok()
        || (!host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            }));
    if !valid_host || settings.port == 0 {
        return Err(AppError::new(
            "invalid_smtp_host",
            "请填写有效 SMTP 服务器和非零端口，不要填写协议或路径。",
        ));
    }
    if settings.tls_mode != "tls" && settings.tls_mode != "starttls" {
        return Err(AppError::new(
            "invalid_tls_mode",
            "仅支持 TLS 或必须升级的 STARTTLS，不支持明文连接。",
        ));
    }
    if settings.username.trim().is_empty() || settings.password.is_empty() {
        return Err(AppError::new(
            "missing_smtp_credentials",
            "请填写 SMTP 账号和密码或邮箱授权码。",
        ));
    }
    if settings.username.len() > 1024
        || settings.password.len() > 4096
        || settings.username.chars().any(char::is_control)
    {
        return Err(AppError::new(
            "invalid_smtp_credentials",
            "SMTP 账号或授权码格式无效，请重新输入。",
        ));
    }
    if settings.from_name.chars().any(char::is_control) || settings.from_name.chars().count() > 256
    {
        return Err(AppError::new(
            "invalid_from_name",
            "发件人姓名不能包含换行、控制字符或过长内容。",
        ));
    }
    Ok(())
}

fn numeric_qq(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|c| c.is_ascii_digit())
        && value.bytes().any(|c| c != b'0')
}

/// 文件名不能引入路径或 MIME 头换行，Unicode 显示名称仍保留给邮件客户端。
fn attachment_name(input: &str) -> String {
    let base = input.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if "<>:\"|?*".contains(c) { '_' } else { c })
        .take(180)
        .collect();
    let name = name.trim().trim_matches('.');
    if name.is_empty() {
        "attachment.bin".into()
    } else {
        name.into()
    }
}

/// 纯文本版本用于不渲染 HTML 的邮箱客户端；正则只处理已经白名单清理过的 HTML。
fn plain_alternative(html: &str) -> String {
    static BREAKS: OnceLock<Regex> = OnceLock::new();
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let breaks = BREAKS.get_or_init(|| {
        Regex::new(r"(?i)<br\s*/?>|</(?:p|div|li|h[1-6]|blockquote|tr)>")
            .expect("constant line-break regex")
    });
    let tags = TAGS.get_or_init(|| Regex::new(r"<[^>]*>").expect("constant tag regex"));
    let separated = breaks.replace_all(html, "\n");
    let plain = tags.replace_all(&separated, "");
    html_escape::decode_html_entities(&plain).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_accounts_keep_order_and_cover_every_recipient_once() {
        for (recipients, expected) in [
            (0, vec![0, 0, 0, 0, 0]),
            (1, vec![1, 0, 0, 0, 0]),
            (2, vec![1, 1, 0, 0, 0]),
            (3, vec![1, 1, 1, 0, 0]),
            (5, vec![1, 1, 1, 1, 1]),
            (8, vec![2, 2, 2, 1, 1]),
            (10, vec![2, 2, 2, 2, 2]),
        ] {
            let ranges = balanced_recipient_ranges(recipients, 5).unwrap();
            assert_eq!(
                ranges.iter().map(|range| range.len()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                ranges.into_iter().flatten().collect::<Vec<_>>(),
                (0..recipients).collect::<Vec<_>>()
            );
        }
        assert_eq!(
            balanced_recipient_ranges(1, 0).unwrap_err().code,
            "mail_accounts_empty"
        );
    }

    #[test]
    fn account_allocation_uses_normalized_filtered_recipients_only() {
        // 重复候选先在解析阶段去除，再模拟已发送及手动排除过滤；分配不能从原始十行估算。
        let parsed = parse_recipients("first@example.test;first@EXAMPLE.TEST;sent@example.test;blocked@example.test;second@example.test;third@example.test").unwrap();
        let eligible = parsed
            .into_iter()
            .map(|item| item.email)
            .filter(|email| !matches!(email.as_str(), "sent@example.test" | "blocked@example.test"))
            .collect::<Vec<_>>();
        let ranges = balanced_recipient_ranges(eligible.len(), 2).unwrap();
        assert_eq!(
            &eligible[ranges[0].clone()],
            &["first@example.test", "second@example.test"]
        );
        assert_eq!(&eligible[ranges[1].clone()], &["third@example.test"]);
    }

    /// 仅监听随机本机端口，在 SMTP 问候阶段即拒绝；不支持 AUTH/MAIL/RCPT/DATA，不会产生邮件。
    /// 用真实 lettre 错误对象检验诊断链，避免测试用手造字符串绕过实际错误格式与状态码解析。
    fn rejecting_transport(
        reply: &'static str,
    ) -> (
        AsyncSmtpTransport<Tokio1Executor>,
        std::thread::JoinHandle<()>,
    ) {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, peer) = listener.accept().unwrap();
            assert!(peer.ip().is_loopback());
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
        });
        // 明文连接仅存在于本机问候拒绝的测试；生产构造器仍只允许 TLS/STARTTLS。
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
            .port(port)
            .timeout(Some(Duration::from_secs(2)))
            .build();
        (transport, server)
    }

    #[tokio::test]
    async fn smtp_diagnostics_preserve_server_status_and_redact_every_known_identity() {
        let (transport, server) = rejecting_transport(
            "550-5.7.1 denied sender@example.com fixture-only-password\r\n550 5.7.1 receiver@example.test 测试发件人 policy\r\n",
        );
        let error = transport.test_connection().await.unwrap_err();
        server.join().unwrap();
        let sender = MailSender::new(&fixture_settings()).unwrap();
        let details = sender.error_details(&error, &["receiver@example.test".into()]);
        assert_eq!(details["category"], "smtp_permanent");
        assert_eq!(details["smtp_status"], "550");
        assert_eq!(details["extended_status"], json!(["5.7.1"]));
        assert!(details["error_chain"].as_array().unwrap().len() >= 2);
        let diagnostic = details.to_string();
        assert!(diagnostic.contains("policy"));
        for secret in [
            "sender@example.com",
            "fixture-only-password",
            "receiver@example.test",
            "测试发件人",
        ] {
            assert!(
                !diagnostic.contains(secret),
                "日志诊断泄露合成身份：{diagnostic}"
            );
        }
    }

    #[tokio::test]
    async fn smtp_transient_diagnostics_do_not_change_unknown_delivery_semantics() {
        let (transport, server) = rejecting_transport("421 4.7.0 temporarily unavailable\r\n");
        let error = transport.test_connection().await.unwrap_err();
        server.join().unwrap();
        let details = smtp_error_details(&error, &[]);
        assert_eq!(details["category"], "smtp_transient");
        assert_eq!(details["smtp_status"], "421");
        assert_eq!(details["extended_status"], json!(["4.7.0"]));

        let (transport, server) = rejecting_transport("421 4.7.0 temporarily unavailable\r\n");
        let mut sender = MailSender::new(&fixture_settings()).unwrap();
        sender.transport = transport;
        let outcome = sender
            .send_batch_with_context(
                &["receiver@example.test".into()],
                "不发送的合成主题",
                "<p>不会离开本机</p>",
                &[],
                Some(MailSendContext {
                    job_id: "synthetic-job",
                    batch_index: 1,
                }),
            )
            .await
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(outcome.code, "mail_delivery_unknown");
        assert_eq!(
            outcome.message,
            "未能确认本批邮件的投递结果，请先核实邮箱或服务商记录；请勿直接拆分重发。"
        );
    }

    /// 真实 SMTP 拒绝响应仅在本机问候阶段产生；验证固定提示精确匹配而非单看 550 或响应关键词。
    #[tokio::test]
    async fn smtp_frequency_hint_requires_both_550_and_explicit_too_many_attempts() {
        for (reply, limited) in [
            ("550 Too many attempts\r\n", true),
            (
                "550 5.7.1 TOO   MANY ATTEMPTS receiver@example.test\r\n",
                true,
            ),
            ("550 5.1.1 mailbox unavailable\r\n", false),
            ("535 5.7.8 Too many attempts\r\n", false),
            ("421 4.7.0 Too many attempts\r\n", false),
            ("552 Too many attempts\r\n", false),
            ("not an SMTP response: Too many attempts\r\n", false),
        ] {
            let (transport, server) = rejecting_transport(reply);
            let error = transport.test_connection().await.unwrap_err();
            server.join().unwrap();
            for batch in [false, true] {
                let message = delivery_error_message(&error, batch);
                assert_eq!(
                    message.starts_with("发送过于频繁，请稍后再试。"),
                    limited,
                    "unexpected hint for {reply}"
                );
                assert!(message.contains("核实"));
                assert!(!message.contains("receiver@example.test"));
                if limited {
                    assert!(message.contains("避免重复发送"));
                }
            }
        }
    }

    /// 固定文案替换必须沿真实 send 路径返回原 unknown 错误；本机拒绝发生在任何邮件命令之前。
    #[tokio::test]
    async fn smtp_frequency_hint_keeps_unknown_code_and_existing_log_classification() {
        let (transport, server) = rejecting_transport("550 5.7.1 Too many attempts\r\n");
        let error = transport.test_connection().await.unwrap_err();
        server.join().unwrap();
        let details = smtp_error_details(&error, &[]);
        assert_eq!(details["category"], "smtp_permanent");
        assert_eq!(details["smtp_status"], "550");
        assert!(details["error_chain"]
            .to_string()
            .contains("Too many attempts"));

        let (transport, server) = rejecting_transport("550 5.7.1 Too many attempts\r\n");
        let mut sender = MailSender::new(&fixture_settings()).unwrap();
        sender.transport = transport;
        let error = sender
            .send_batch(
                &["receiver@example.test".into()],
                "合成主题",
                "<p>不会发送的正文</p>",
                &[],
            )
            .await
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.code, "mail_delivery_unknown");
        assert_eq!(
            error.message,
            "发送过于频繁，请稍后再试。请先核实本批投递记录，避免重复发送。"
        );
    }

    /// 动态生成无个人信息的 1×1 PNG，验证真实图片头和 MIME；不依赖本机截图文件。
    fn fixture_inline_image() -> MailAttachment {
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        MailAttachment {
            name: "测试截图.png".into(),
            bytes: output.into_inner(),
            inline: Some(InlineImageMetadata {
                content_id: "fixture-image@inline.local".into(),
                mime_type: "image/png".into(),
            }),
        }
    }

    fn fixture_settings() -> MailSettings {
        MailSettings {
            host: "smtp.example.com".into(),
            port: 465,
            tls_mode: "tls".into(),
            username: "sender@example.com".into(),
            password: "fixture-only-password".into(),
            from_address: "sender@example.com".into(),
            from_name: "测试发件人".into(),
        }
    }

    #[test]
    fn import_deduplicates_domain_case_and_only_maps_numeric_qq_addresses() {
        let entries =
            parse_recipients("12345@QQ.COM；12345@qq.com\nname@example.com,alias@qq.com").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].email, "12345@qq.com");
        assert_eq!(entries[0].qq_number.as_deref(), Some("12345"));
        assert!(entries[1].qq_number.is_none());
        assert!(entries[2].qq_number.is_none());
        assert!(parse_recipients("valid@example.com;broken-address").is_err());
        assert!(parse_recipients(" ").unwrap().is_empty());
    }

    #[test]
    fn body_removes_executable_content_and_remote_images() {
        let safe = sanitize_body(
            r#"<p onclick="x()">你好<b>加粗</b></p><script>secret()</script><img src="https://tracker.test/pixel"><a href="javascript:x()">坏链接</a><a href="https://example.com">安全链接</a>"#,
        );
        assert!(safe.contains("你好<b>加粗</b>"));
        assert!(!safe.contains("onclick"));
        assert!(!safe.contains("script"));
        assert!(!safe.contains("secret()"));
        assert!(!safe.contains("<img"));
        assert!(!safe.contains("javascript:"));
        assert!(safe.contains("https://example.com"));
        assert_eq!(
            plain_alternative("<p>你好 &amp; A</p><p>B</p>"),
            "你好 & A\nB"
        );
    }

    /// 正文图必须作为 related 部件真实编码；密送名单不能因加入图片而出现在 MIME 头部。
    #[test]
    fn inline_image_is_embedded_with_cid_and_keeps_batch_private() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let image = fixture_inline_image();
        let sender = MailSender::new(&fixture_settings()).unwrap();
        let message = sender.build_batch_message(
            &["first@example.com".into(), "second@example.com".into()],
            "图片测试",
            r#"<p>截图</p><img src="cid:fixture-image@inline.local" onerror="bad()"><img src="https://tracker.test/image"><img src="blob:local-secret"><img src="data:image/png;base64,bad"><img src="cid:unknown"><a href="cid:fixture-image@inline.local">链接</a>"#,
            std::slice::from_ref(&image),
        ).unwrap();
        assert_eq!(message.envelope().to().len(), 2);
        let mime = String::from_utf8(message.formatted()).unwrap();
        assert!(mime.contains("multipart/alternative"));
        assert!(mime.contains("multipart/related"));
        assert!(mime.contains("Content-Type: image/png"));
        assert!(mime.contains("Content-ID: <fixture-image@inline.local>"));
        assert!(mime.contains("Content-Disposition: inline"));
        // MIME base64 每 76 列换行，移除换行后确认所有图片字节确实随邮件发送。
        assert!(mime
            .replace("\r\n", "")
            .contains(&STANDARD.encode(&image.bytes)));
        for forbidden in [
            "Bcc:",
            "first@example.com",
            "second@example.com",
            "tracker.test",
            "blob:",
            "data:image",
            "cid:unknown",
            "onerror",
            "bad()",
        ] {
            assert!(
                !mime.contains(forbidden),
                "unexpected MIME content: {forbidden}"
            );
        }
        let ids = ["fixture-image@inline.local".into()].into_iter().collect();
        let safe = sanitize_body_with_images(
            r#"<img src="cid:fixture-image@inline.local"><a href="cid:fixture-image@inline.local">x</a>"#,
            &ids,
        );
        assert!(safe.contains("src=\"cid:fixture-image@inline.local\""));
        assert!(!safe.contains("href=\"cid:"));
    }

    /// CID 映射只作用于解析后的图片属性；正文里的完整 src 示例不变，也不能顺次替换串到下一张图。
    #[test]
    fn image_cid_mapping_changes_only_registered_img_src_attributes() {
        let mapping = [
            ("old@inline.local".into(), "second@inline.local".into()),
            ("second@inline.local".into(), "new@inline.local".into()),
        ]
        .into_iter()
        .collect();
        let html = r#"<p>示例：src="cid:old@inline.local"</p><code>src="cid:second@inline.local"</code><img src="cid:old@inline.local" alt='src="cid:old@inline.local"'><img src="cid:second@inline.local"><img src="cid:missing@inline.local"><img src="https://tracker.example/test"><a href="cid:old@inline.local">链接</a><script>bad()</script>"#;
        let safe = sanitize_body_with_image_mapping(html, &mapping);
        assert!(safe.contains(r#"<p>示例：src="cid:old@inline.local"</p>"#));
        assert!(safe.contains(r#"<code>src="cid:second@inline.local"</code>"#));
        assert!(safe.contains(r#"<img src="cid:second@inline.local""#));
        assert!(safe.contains(r#"<img src="cid:new@inline.local""#));
        assert!(safe.contains("alt=\"src=&quot;cid:old@inline.local&quot;\""));
        for forbidden in [
            "cid:missing",
            "https://tracker",
            "href=\"cid:",
            "script",
            "bad()",
        ] {
            assert!(!safe.contains(forbidden), "{forbidden}");
        }
    }

    /// 图片内容、类型、CID、数量和共有大小必须在 SMTP / 发送历史操作之前拒绝。
    #[test]
    fn inline_images_enforce_type_identity_count_and_shared_size_limits() {
        let sender = MailSender::new(&fixture_settings()).unwrap();
        let image = fixture_inline_image();
        assert!(validate_inline_image(&image.bytes, "image/png").is_ok());
        assert!(validate_inline_image(&image.bytes, "image/jpeg").is_err());
        assert!(validate_inline_image(b"<svg></svg>", "image/svg+xml").is_err());
        assert!(validate_inline_image(b"\x89PNG\r\n\x1a\n", "image/png").is_err());
        assert!(validate_inline_image(&vec![0; MAX_INLINE_IMAGE_BYTES + 1], "image/png").is_err());
        let error = sender
            .validate_draft("a@example.com", "测试", "", &[image.clone(), image.clone()])
            .unwrap_err();
        assert_eq!(error.code, "inline_image_id");
        let mut injected = image.clone();
        injected.inline.as_mut().unwrap().content_id = "evil\r\nInjected: x".into();
        assert_eq!(
            sender
                .validate_draft("a@example.com", "测试", "", &[injected])
                .unwrap_err()
                .code,
            "inline_image_id"
        );
        assert_eq!(
            sender
                .validate_draft(
                    "a@example.com",
                    "测试",
                    "",
                    &vec![image.clone(); MAX_INLINE_IMAGES + 1]
                )
                .unwrap_err()
                .code,
            "too_many_attachments"
        );
        let ordinary = MailAttachment {
            name: "普通附件.bin".into(),
            bytes: vec![0; MAX_ATTACHMENT_BYTES],
            inline: None,
        };
        assert_eq!(
            sender
                .validate_draft("a@example.com", "测试", "", &[ordinary, image])
                .unwrap_err()
                .code,
            "attachments_too_large"
        );
        let mut wide_image = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8193, 1)
            .write_to(&mut wide_image, image::ImageFormat::Png)
            .unwrap();
        assert_eq!(
            validate_inline_image(wide_image.get_ref(), "image/png")
                .unwrap_err()
                .code,
            "inline_image_dimensions"
        );
        // 使用真实编码验证全部承诺格式可读，而不是只检查扩展名或 MIME 字符串。
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::Gif, "image/gif"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let mut encoded = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 2)
                .write_to(&mut encoded, format)
                .unwrap();
            assert!(validate_inline_image(encoded.get_ref(), mime).is_ok());
        }
    }

    #[test]
    fn smtp_requires_verified_secure_transport_configuration() {
        let mut settings = fixture_settings();
        settings.tls_mode = "none".into();
        assert!(MailSender::new(&settings).is_err());
        settings.tls_mode = "starttls".into();
        settings.port = 587;
        assert!(MailSender::new(&settings).is_ok());
        settings.host = "https://smtp.example.com/path".into();
        assert!(MailSender::new(&settings).is_err());
        settings.host = "smtp.example.com".into();
        settings.password.clear();
        assert!(MailSender::new(&settings).is_err());
    }

    #[test]
    fn message_is_single_recipient_with_unicode_text_and_safe_attachment() {
        let sender = MailSender::new(&fixture_settings()).unwrap();
        let message = sender
            .build_message(
                "receiver@example.com",
                "测试主题",
                "<p>你好 😀</p>",
                &[MailAttachment {
                    name: "../../测试\r\n附件.txt".into(),
                    bytes: "附件内容".as_bytes().to_vec(),
                    inline: None,
                }],
            )
            .unwrap();
        let formatted = String::from_utf8(message.formatted()).unwrap();
        assert!(formatted.contains("To: receiver@example.com"));
        assert!(!formatted.contains("Cc:"));
        assert!(!formatted.contains("Bcc:"));
        assert!(formatted.contains("multipart/alternative"));
        assert!(formatted.contains("text/plain"));
        assert!(formatted.contains("text/html"));
        assert!(formatted.contains("application/octet-stream"));
        assert!(!formatted.contains("../../"));
        assert_eq!(attachment_name("../../测试\r\n附件.txt"), "测试附件.txt");
        assert!(sender
            .build_message(
                "receiver@example.com",
                "subject\r\nBcc: other@example.com",
                "",
                &[]
            )
            .is_err());
    }

    #[test]
    fn batch_has_multiple_envelope_recipients_without_exposing_bcc_headers() {
        let sender = MailSender::new(&fixture_settings()).unwrap();
        let recipients = vec![
            "first@example.com".into(),
            "second@example.com".into(),
            "first@EXAMPLE.COM".into(),
        ];
        let message = sender
            .build_batch_message(&recipients, "密送测试", "<p>同一封内容</p>", &[])
            .unwrap();
        assert_eq!(message.envelope().to().len(), 2);
        let formatted = String::from_utf8(message.formatted()).unwrap();
        assert!(!formatted.contains("Bcc:"));
        assert!(!formatted.contains("To:"));
        assert!(!formatted.contains("first@example.com"));
        assert!(!formatted.contains("second@example.com"));
        assert!(sender.build_batch_message(&[], "空名单", "", &[]).is_err());
        assert!(sender
            .build_batch_message(
                &vec!["a@example.com".into(); MAX_BATCH_RECIPIENTS + 1],
                "过多",
                "",
                &[]
            )
            .is_err());
    }

    /// 明确的本地草稿错误必须在建立 sending 记录前拒绝，不能混淆成 SMTP 结果不明。
    #[test]
    fn local_draft_validation_rejects_invalid_subject_and_oversized_content() {
        let sender = MailSender::new(&fixture_settings()).unwrap();
        assert!(sender
            .validate_draft("receiver@example.com", "正常主题", "<p>正文</p>", &[])
            .is_ok());
        assert_eq!(
            sender
                .validate_draft("receiver@example.com", "主题\r\nInjected: value", "", &[])
                .unwrap_err()
                .code,
            "invalid_subject"
        );
        assert_eq!(
            sender
                .validate_draft("receiver@example.com", &"a".repeat(999), "", &[])
                .unwrap_err()
                .code,
            "invalid_subject"
        );

        let html = "a".repeat(MAX_BODY_BYTES + 1);
        assert_eq!(
            sender
                .validate_draft("receiver@example.com", "主题", &html, &[])
                .unwrap_err()
                .code,
            "body_too_large"
        );
        drop(html);
        let attachment = MailAttachment {
            name: "oversized.bin".into(),
            bytes: vec![0; MAX_ATTACHMENT_BYTES + 1],
            inline: None,
        };
        assert_eq!(
            sender
                .validate_draft("receiver@example.com", "主题", "", &[attachment])
                .unwrap_err()
                .code,
            "attachments_too_large"
        );
    }
}
