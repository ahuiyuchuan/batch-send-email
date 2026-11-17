//! 从固定快照生成文件，防止筛选漏导、Excel 公式解释和半成品覆盖。作者：ahui。

use crate::models::{AppError, AppResult, FriendRelation, Snapshot};
use chrono::{DateTime, Local};
use rust_xlsxwriter::{DocProperties, Format, Workbook};
use std::{io::Write, path::Path};
use tempfile::NamedTempFile;

/// 用户主动选择的文件格式；只有本机保存对话框产生的路径可以传入导出函数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// Excel 工作簿，所有号码、昵称和名片均明确写为文本。
    Xlsx,
    /// UTF-8 BOM 与 CRLF；昵称、名片的危险公式前缀会额外添加单引号。
    Csv,
    /// UTF-8 无 BOM 与 CRLF，只包含数字 QQ 号，一行一个。
    Txt,
}

impl ExportFormat {
    /// 解析界面格式标识；其他值返回稳定错误，不静默采用默认格式。
    pub fn parse(input: &str) -> AppResult<Self> {
        match input {
            "xlsx" => Ok(Self::Xlsx),
            "csv" => Ok(Self::Csv),
            "txt" => Ok(Self::Txt),
            _ => Err(AppError::new(
                "invalid_format",
                "不支持的导出格式，请重新选择。",
            )),
        }
    }

    /// 返回不带点的扩展名，供本机保存对话框和文件名使用。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Txt => "txt",
        }
    }
}

/// 生成不包含昵称或任意路径字符的默认文件名，时间按运行机器的本地时区显示。
///
/// 单群只使用数字群号，多群使用固定“多群合并”标记，另附获取时间及结果状态。
/// 非法单群号使用固定占位，避免昵称、组合键或任意路径字符进入默认文件名。
pub fn default_filename(snapshot: &Snapshot, format: ExportFormat) -> String {
    let group_id = if !snapshot.source_groups.is_empty() {
        "多群合并"
    } else if !snapshot.group.id.is_empty()
        && snapshot.group.id.len() <= 20
        && snapshot.group.id.bytes().all(|byte| byte.is_ascii_digit())
    {
        snapshot.group.id.as_str()
    } else {
        "未知群号"
    };
    let timestamp = DateTime::parse_from_rfc3339(&snapshot.finished_at)
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y%m%d_%H%M%S")
                .to_string()
        })
        .unwrap_or_else(|_| "未知时间".into());
    let status = match snapshot.status.as_str() {
        "matched" => "数量一致",
        "partial" => "部分",
        _ => "待核验",
    };
    let exclusion_label = if snapshot.exclusions.is_empty()
        && !snapshot.exclude_friends
        && !snapshot.exclude_sent
        && snapshot.manual_filtered_count == 0
    {
        ""
    } else if snapshot
        .exclusions
        .iter()
        .any(|summary| summary.status != "matched")
    {
        "_排除待核验"
    } else {
        "_已过滤"
    };
    format!(
        "QQ成员_{group_id}_{timestamp}_{status}{exclusion_label}.{}",
        format.extension()
    )
}

/// 将当前全部结果原子保存到用户选择的位置，失败时保留已有目标文件。
///
/// `path` 必须由宿主的本机保存对话框提供，前端不可自由指定路径。
/// 零成员、格式不符、目录不可写、临时文件或替换失败会返回不包含路径及成员数据的错误。
/// 本函数只使用快照，不读取页面搜索条件，也不再次请求 QQ。
pub fn write_export(snapshot: &Snapshot, path: &Path, format: ExportFormat) -> AppResult<()> {
    crate::verify::validate_friend_filter(snapshot)?;
    let entirely_excluded = snapshot.source_count > 0
        && snapshot.filtered_members.len() == snapshot.source_count
        && snapshot.excluded_count
            + snapshot.friend_filtered_count
            + snapshot.sent_filtered_count
            + snapshot.manual_filtered_count
            == snapshot.source_count;
    if snapshot.members.is_empty() && !entirely_excluded {
        return Err(AppError::new(
            "empty_export",
            "当前没有可导出的有效成员，请先获取名单。",
        ));
    }
    let parent = path
        .parent()
        .filter(|directory| directory.is_dir())
        .ok_or_else(|| {
            AppError::new("invalid_destination", "保存位置不可用，请重新选择文件夹。")
        })?;
    if path.is_dir() {
        return Err(AppError::new(
            "invalid_destination",
            "请选择文件名，不能将文件夹作为导出文件。",
        ));
    }
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|_| AppError::new("export_write", "无法创建导出文件，请检查文件夹写入权限。"))?;
    match format {
        ExportFormat::Xlsx => write_xlsx(snapshot, temporary.as_file_mut())?,
        ExportFormat::Csv => write_csv(snapshot, temporary.as_file_mut())?,
        ExportFormat::Txt => {
            for member in &snapshot.members {
                writeln_crlf(temporary.as_file_mut(), &member.qq_number)?;
            }
        }
    }
    temporary.as_file_mut().flush().map_err(|_| write_error())?;
    temporary.as_file().sync_all().map_err(|_| write_error())?;

    // 临时文件位于同一目录，使用操作系统替换目标；只有完整写入后才会触碰旧文件。
    temporary.persist(path).map_err(|_| {
        AppError::new(
            "export_replace",
            "文件已生成但保存失败，请关闭占用该文件的程序，或另选文件名。",
        )
    })?;
    Ok(())
}

/// 以 XLSX 的显式字符串类型保存内容，因此以等号开头的昵称也不会成为公式。
fn write_xlsx(snapshot: &Snapshot, file: &mut std::fs::File) -> AppResult<()> {
    let mut workbook = Workbook::new();
    workbook.set_properties(
        &DocProperties::new()
            .set_author("ahui")
            .set_title("QQ 群成员名单"),
    );
    let header = Format::new().set_bold().set_background_color(0xEAF1FFu32);
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("成员名单").map_err(|_| write_error())?;
    for (column, label) in ["QQ号", "QQ昵称", "群名片", "与获取账号的好友关系"]
        .iter()
        .enumerate()
    {
        worksheet
            .write_string_with_format(0, column as u16, *label, &header)
            .map_err(|_| write_error())?;
    }
    worksheet
        .set_column_width(0, 22)
        .map_err(|_| write_error())?;
    worksheet
        .set_column_width(1, 28)
        .map_err(|_| write_error())?;
    worksheet
        .set_column_width(2, 36)
        .map_err(|_| write_error())?;
    worksheet
        .set_column_width(3, 24)
        .map_err(|_| write_error())?;
    worksheet
        .set_freeze_panes(1, 0)
        .map_err(|_| write_error())?;
    for (index, member) in snapshot.members.iter().enumerate() {
        let row = u32::try_from(index + 1).map_err(|_| write_error())?;
        for (column, value) in [
            member.qq_number.as_str(),
            member.nickname.as_str(),
            member.card.as_str(),
            friend_relation_label(member.friend_relation),
        ]
        .iter()
        .enumerate()
        {
            worksheet
                .write_string(row, column as u16, *value)
                .map_err(|_| write_error())?;
        }
    }
    if !snapshot.members.is_empty() {
        worksheet
            .autofilter(0, 0, snapshot.members.len() as u32, 3)
            .map_err(|_| write_error())?;
    }
    if !snapshot.filtered_members.is_empty()
        || snapshot.exclude_sent
        || snapshot.exclude_friends
        || !snapshot.exclusions.is_empty()
    {
        let filtered = workbook.add_worksheet();
        filtered.set_name("已过滤").map_err(|_| write_error())?;
        for (column, label) in [
            "QQ号",
            "QQ昵称",
            "群名片",
            "过滤原因",
            "说明",
            "与获取账号的好友关系",
        ]
        .iter()
        .enumerate()
        {
            filtered
                .write_string_with_format(0, column as u16, *label, &header)
                .map_err(|_| write_error())?;
        }
        for (column, width) in [(0, 22), (1, 28), (2, 36), (3, 20), (4, 40), (5, 24)] {
            filtered
                .set_column_width(column, width)
                .map_err(|_| write_error())?;
        }
        filtered.set_freeze_panes(1, 0).map_err(|_| write_error())?;
        for (index, item) in snapshot.filtered_members.iter().enumerate() {
            let reason = match item.reason.as_str() {
                "group" => "与排除群重合",
                "friend" => "已是获取账号好友",
                "sent" => "历史已发送",
                "manual" => "手动排除",
                _ => "历史过滤",
            };
            for (column, value) in [
                item.member.qq_number.as_str(),
                item.member.nickname.as_str(),
                item.member.card.as_str(),
                reason,
                item.detail.as_str(),
                friend_relation_label(item.member.friend_relation),
            ]
            .iter()
            .enumerate()
            {
                filtered
                    .write_string((index + 1) as u32, column as u16, *value)
                    .map_err(|_| write_error())?;
            }
        }
        if !snapshot.filtered_members.is_empty() {
            filtered
                .autofilter(0, 0, snapshot.filtered_members.len() as u32, 5)
                .map_err(|_| write_error())?;
        }
    }
    let notes = workbook.add_worksheet();
    notes.set_name("获取说明").map_err(|_| write_error())?;
    notes.set_column_width(0, 23).map_err(|_| write_error())?;
    notes.set_column_width(1, 90).map_err(|_| write_error())?;
    let source = match snapshot.account.source.as_str() {
        "web" => "网页方式",
        // 旧快照仍能导出，但不能将其来源误标为当前网页读取结果。
        _ => "历史来源",
    };
    // 旧快照新增计数字段默认零，但关系本身为未知；直接统计保留的完整目标，避免误报“未知 0 人”。
    let friend_unknown_count = snapshot
        .members
        .iter()
        .chain(snapshot.filtered_members.iter().map(|item| &item.member))
        .filter(|member| member.friend_relation == FriendRelation::Unknown)
        .count();
    let mut metadata = vec![
        ("群号", if snapshot.source_groups.is_empty() { snapshot.group.id.clone() } else { snapshot.source_groups.iter().map(|source| source.group.id.as_str()).collect::<Vec<_>>().join("、") }),
        ("群名称", snapshot.group.name.clone()),
        ("获取账号", snapshot.account.qq_number.clone()),
        ("获取来源", source.to_string()),
        ("开始时间", snapshot.started_at.clone()),
        ("结束时间", snapshot.finished_at.clone()),
        ("目标群有效人数", snapshot.source_count.to_string()),
        ("实际排除人数", snapshot.excluded_count.to_string()),
        (
            "好友过滤",
            if snapshot.exclude_friends {
                "已完成"
            } else {
                "未启用"
            }
            .into(),
        ),
        ("好友过滤人数", snapshot.friend_filtered_count.to_string()),
        ("好友关系未知人数", friend_unknown_count.to_string()),
        ("已发送过滤人数", snapshot.sent_filtered_count.to_string()),
        ("手动排除人数", snapshot.manual_filtered_count.to_string()),
        ("手动排除版本", snapshot.exclusion_revision.to_string()),
        ("已过滤总人数", snapshot.filtered_members.len().to_string()),
        ("最终导出人数", snapshot.members.len().to_string()),
        (
            "参考人数",
            snapshot
                .reported_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "未知".into()),
        ),
        (if snapshot.restoration.is_some() { "原获取核验结果" } else { "核验结果" }, snapshot.message.clone()),
        ("去重条数", snapshot.duplicate_count.to_string()),
        ("无效号码条数", snapshot.invalid_count.to_string()),
        (
            "名单边界",
            "群成员以原获取时为准；手动排除按导出所用版本叠加，不会重新读取 QQ。数量一致不保证获取期间没有等量进退群。".into(),
        ),
    ];
    if let Some(restoration) = &snapshot.restoration {
        metadata.extend([
            ("名单调整时间", restoration.adjusted_at.clone()),
            ("调整前版本", restoration.parent_snapshot_id.clone()),
            ("本名单恢复人数", restoration.members.len().to_string()),
            ("恢复边界", "仅对当前名单生效，重新获取会重置；群重合统计为原获取时结果，当前过滤人数已扣除恢复项。发送历史、全局排除和未决投递不受恢复影响。".into()),
        ]);
        for entry in &restoration.members {
            let reason = match entry.reason.as_str() {
                "group" => "排除群重合",
                "friend" => "QQ 好友",
                "sent" => "已发送记录",
                _ => "未知",
            };
            metadata.push((
                "已恢复成员",
                format!(
                    "{}；原因为{}；{}",
                    entry.member.qq_number, reason, entry.detail
                ),
            ));
        }
    }
    metadata.extend(
        snapshot
            .warnings
            .iter()
            .map(|warning| ("补充说明", warning.clone())),
    );
    for source in &snapshot.source_groups {
        metadata.push((
            "来源群",
            format!(
                "{}（{}）；合法去重{}人，参考{}，核验{}",
                source.group.name,
                source.group.id,
                source.qq_numbers.len(),
                source
                    .reported_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "未知".into()),
                source.status
            ),
        ));
    }
    for summary in &snapshot.exclusions {
        metadata.push((
            "排除群",
            format!(
                "{}（{}）；取得 {} 人，参考人数 {}，状态 {}，原获取时本群新增排除 {} 人",
                summary.group.name,
                summary.group.id,
                summary.acquired_count,
                summary
                    .reported_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "未知".into()),
                match summary.status.as_str() {
                    "matched" => "数量一致",
                    "partial" => "名单不完整",
                    _ => "待核验",
                },
                summary.removed_count,
            ),
        ));
    }
    for (row, (label, value)) in metadata.iter().enumerate() {
        notes
            .write_string(row as u32, 0, *label)
            .map_err(|_| write_error())?;
        notes
            .write_string(row as u32, 1, value)
            .map_err(|_| write_error())?;
    }
    workbook.save_to_writer(file).map_err(|_| write_error())?;
    Ok(())
}

/// 好友关系只相对于获取账号成立；历史名单缺失关系时明确输出未知，避免暗示已核实。
fn friend_relation_label(relation: FriendRelation) -> &'static str {
    match relation {
        FriendRelation::Friend => "好友",
        FriendRelation::NotFriend => "非好友",
        FriendRelation::Unknown => "未知",
    }
}

/// CSV 保留标准引号转义；仅对可被 Excel 解释为公式的文本增加单引号前缀。
fn write_csv(snapshot: &Snapshot, file: &mut std::fs::File) -> AppResult<()> {
    file.write_all(&[0xEF, 0xBB, 0xBF])
        .map_err(|_| write_error())?;
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(file);
    writer
        .write_record(["QQ号", "QQ昵称", "群名片"])
        .map_err(|_| write_error())?;
    for member in &snapshot.members {
        writer
            .write_record([
                member.qq_number.as_str(),
                safe_csv_text(&member.nickname).as_str(),
                safe_csv_text(&member.card).as_str(),
            ])
            .map_err(|_| write_error())?;
    }
    writer.flush().map_err(|_| write_error())?;
    Ok(())
}

/// 防护前导空白、控制字符或 BOM 隐藏的公式入口；原文只增加前缀，不删除内容。
fn safe_csv_text(value: &str) -> String {
    let trimmed = value.trim_start_matches(|character: char| {
        character.is_whitespace() || character.is_control() || character == '\u{FEFF}'
    });
    let dangerous_prefix = trimmed.starts_with(['=', '+', '-', '@']);
    let control_prefix = value.starts_with(['\t', '\r', '\n']);
    if dangerous_prefix || control_prefix {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

fn writeln_crlf(file: &mut std::fs::File, value: &str) -> AppResult<()> {
    file.write_all(value.as_bytes())
        .map_err(|_| write_error())?;
    file.write_all(b"\r\n").map_err(|_| write_error())?;
    Ok(())
}

/// 不把库错误的路径、单元格内容或用户数据反射到页面或日志。
fn write_error() -> AppError {
    AppError::new(
        "export_write",
        "导出文件写入失败，请检查可用磁盘空间和文件夹权限。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Account, FetchResult, Group, Member},
        verify::create_snapshot,
    };
    use std::{
        fs,
        io::{Cursor, Read},
    };

    fn fixture() -> Snapshot {
        create_snapshot(
            "export-test".into(),
            Account {
                qq_number: "10001".into(),
                nickname: "测试账号".into(),
                source: "web".into(),
            },
            Group {
                id: "123456".into(),
                name: "中文🙂群".into(),
                member_count: Some(2),
                role: "member".into(),
            },
            FetchResult {
                members: vec![
                    Member {
                        qq_number: "12345678901234567890".into(),
                        nickname: "=SUM(1,2)".into(),
                        card: "含\"引号\",逗号\n中文🙂".into(),
                        friend_relation: FriendRelation::NotFriend,
                    },
                    Member {
                        qq_number: "98765".into(),
                        nickname: " \t+危险公式".into(),
                        card: "普通名片".into(),
                        friend_relation: FriendRelation::Friend,
                    },
                ],
                reported_count: Some(2),
                freshness_confirmed: true,
                truncated: false,
                warnings: vec![],
            },
            "2026-11-05T06:00:00Z".into(),
            "2026-11-05T06:00:01Z".into(),
        )
    }

    #[test]
    fn txt_contains_only_numbers_without_bom() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("members.txt");
        write_export(&fixture(), &path, ExportFormat::Txt).unwrap();
        assert_eq!(
            fs::read(path).unwrap(),
            b"98765\r\n12345678901234567890\r\n"
        );
    }

    #[test]
    fn csv_has_bom_quotes_unicode_and_safe_formula_prefixes() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("members.csv");
        write_export(&fixture(), &path, ExportFormat::Csv).unwrap();
        let bytes = fs::read(path).unwrap();
        assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
        let mut reader = csv::Reader::from_reader(&bytes[3..]);
        let records = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(&records[0][1], "' \t+危险公式");
        assert_eq!(&records[1][0], "12345678901234567890");
        assert_eq!(&records[1][1], "'=SUM(1,2)");
        assert_eq!(&records[1][2], "含\"引号\",逗号\n中文🙂");
        assert!(bytes.ends_with(b"\r\n"));
    }

    #[test]
    fn xlsx_uses_text_cells_and_contains_provenance() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("members.xlsx");
        write_export(&fixture(), &path, ExportFormat::Xlsx).unwrap();
        let bytes = fs::read(path).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("t=\"s\""));
        assert!(!sheet.contains("<f>"));
        let mut strings = String::new();
        archive
            .by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut strings)
            .unwrap();
        assert!(strings.contains("12345678901234567890"));
        assert!(strings.contains("=SUM(1,2)"));
        assert!(strings.contains("中文🙂"));
        assert!(strings.contains("数量一致"));
        assert!(strings.contains("2026-11-05T06:00:01Z"));
    }

    /// 历史快照仍可导出，但显示来源不能冒充当前网页获取；原始来源标识保持不变。
    #[test]
    fn historical_source_exports_without_becoming_web_source() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("history.xlsx");
        let mut snapshot = fixture();
        snapshot.account.source = "historical".into();
        write_export(&snapshot, &path, ExportFormat::Xlsx).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(fs::read(path).unwrap())).unwrap();
        let mut strings = String::new();
        archive
            .by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut strings)
            .unwrap();
        assert!(strings.contains("历史来源"));
        assert!(!strings.contains("网页方式"));
        assert_eq!(snapshot.account.source, "historical");
    }

    #[test]
    fn failed_export_preserves_existing_file_and_success_replaces_it() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("members.txt");
        fs::write(&path, b"existing").unwrap();
        let mut empty = fixture();
        empty.members.clear();
        assert!(write_export(&empty, &path, ExportFormat::Txt).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        write_export(&fixture(), &path, ExportFormat::Txt).unwrap();
        assert_ne!(fs::read(path).unwrap(), b"existing");
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 1);
    }

    #[test]
    fn filenames_and_formats_do_not_allow_arbitrary_path_components() {
        assert!(ExportFormat::parse("html").is_err());
        assert_eq!(ExportFormat::parse("xlsx").unwrap().extension(), "xlsx");
        let mut view = fixture();
        view.group.id = "../escape".into();
        view.status = "partial".into();
        let name = default_filename(&view, ExportFormat::Xlsx);
        assert!(!name.contains("../"));
        assert!(name.ends_with("_部分.xlsx"));
    }

    #[test]
    fn entirely_filtered_results_export_empty_txt_and_xlsx_provenance() {
        let folder = tempfile::tempdir().unwrap();
        let original = fixture();
        let sent = original
            .members
            .iter()
            .map(|member| member.qq_number.clone())
            .collect();
        let filtered = crate::verify::apply_sent_filter(original, &sent);
        assert_eq!(filtered.members.len(), 0);
        assert_eq!(filtered.filtered_members.len(), 2);
        let txt = folder.path().join("empty.txt");
        write_export(&filtered, &txt, ExportFormat::Txt).unwrap();
        assert_eq!(fs::read(txt).unwrap(), b"");
        let xlsx = folder.path().join("filtered.xlsx");
        write_export(&filtered, &xlsx, ExportFormat::Xlsx).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(fs::read(xlsx).unwrap())).unwrap();
        let mut workbook = String::new();
        archive
            .by_name("xl/workbook.xml")
            .unwrap()
            .read_to_string(&mut workbook)
            .unwrap();
        assert!(workbook.contains("已过滤"));
        let mut strings = String::new();
        archive
            .by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut strings)
            .unwrap();
        assert!(strings.contains("历史已发送"));
        assert!(strings.contains("最终导出人数"));
        assert!(default_filename(&filtered, ExportFormat::Xlsx).contains("已过滤"));
    }

    /// 好友过滤的空名单仍是合法结果，工作簿保留关系、原因和统计；未完成状态不得覆盖旧文件。
    #[test]
    fn entirely_friend_filtered_results_keep_audit_details_and_guard_incomplete_state() {
        let folder = tempfile::tempdir().unwrap();
        let mut original = fixture();
        for member in &mut original.members {
            member.friend_relation = FriendRelation::Friend;
        }
        let account = original.account.clone();
        let filtered = crate::verify::apply_friend_filter(original, &account).unwrap();
        let txt = folder.path().join("friends.txt");
        write_export(&filtered, &txt, ExportFormat::Txt).unwrap();
        assert_eq!(fs::read(&txt).unwrap(), b"");
        let xlsx = folder.path().join("friends.xlsx");
        write_export(&filtered, &xlsx, ExportFormat::Xlsx).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(fs::read(&xlsx).unwrap())).unwrap();
        let mut strings = String::new();
        archive
            .by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_string(&mut strings)
            .unwrap();
        for expected in [
            "与获取账号的好友关系",
            "已是获取账号好友",
            "好友过滤人数",
            "好友关系未知人数",
            "已完成",
        ] {
            assert!(strings.contains(expected), "{expected}");
        }
        assert!(default_filename(&filtered, ExportFormat::Xlsx).contains("已过滤"));
        let mut incomplete = filtered;
        incomplete.friend_filter_status = "not_requested".into();
        fs::write(&txt, b"existing").unwrap();
        assert_eq!(
            write_export(&incomplete, &txt, ExportFormat::Txt)
                .unwrap_err()
                .code,
            "friend_filter_incomplete"
        );
        assert_eq!(fs::read(txt).unwrap(), b"existing");
    }
}
