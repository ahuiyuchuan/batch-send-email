//! 独立手动排除的宿主边界与快照派生。作者：ahui。
//! 名单只保存在核心数据库；写操作与获取/发送串行，页面、导出和发送均使用当前规则。

use crate::{
    commands::publish,
    state::{AppState, AppView},
};
use member_core::{
    database::{Database, ManualExclusionMutation, ManualExclusionView},
    diagnostics,
    models::*,
};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// 按固定版本读取原始快照并应用当前排除；不会用另一个群或最新版本静默替换指定快照。
pub(crate) fn snapshot_view_for_export(
    database: &Database,
    snapshot_id: &str,
    expected_revision: Option<u64>,
) -> AppResult<Snapshot> {
    if snapshot_id.is_empty() || snapshot_id.len() > 128 {
        return Err(AppError::new("no_result", "请先选择已获取的成员名单。"));
    }
    let raw = database.snapshot_by_id(snapshot_id)?.ok_or_else(|| {
        AppError::new(
            "snapshot_missing",
            "此版本名单已不在本机保留记录中，请重新获取后重试。",
        )
    })?;
    let snapshot = database.apply_manual_exclusions(&raw)?;
    if expected_revision.is_some_and(|revision| revision != snapshot.exclusion_revision) {
        return Err(AppError::new(
            "exclusion_revision_changed",
            "手动排除已更新，请刷新名单后重新导出。",
        ));
    }
    Ok(snapshot)
}

/// 在持有 operation 门闩时调用；所有数据库读取都在后台且不持有 inner/mail 状态锁。
/// 当前展示只保存派生结果，下次刷新始终按同一 id 重读原始快照，移除规则可恢复对应成员。
pub(crate) async fn refresh_current_overlay(state: &AppState) -> AppResult<AppView> {
    let database = state.database()?;
    let snapshot_id = state.view().snapshot.map(|snapshot| snapshot.id);
    let (snapshot, revision) = tokio::task::spawn_blocking(move || {
        let snapshot = snapshot_id
            .as_deref()
            .map(|id| snapshot_view_for_export(&database, id, None))
            .transpose()?;
        // 已派生快照直接沿用同一读快照的版本；空页面才单独读版本，避免跨两个读点拼接状态。
        let revision = match &snapshot {
            Some(snapshot) => snapshot.exclusion_revision,
            None => database.manual_exclusion_revision()?,
        };
        Ok::<_, AppError>((snapshot, revision))
    })
    .await
    .map_err(|_| AppError::new("snapshot_read", "名单刷新中断，请重试。"))??;
    {
        let mut inner = state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.view.snapshot = snapshot;
        inner.view.exclusion_revision = revision;
    }
    Ok(state.view())
}

/// 仅读规则，发送和获取进行中也可查看；不要求 QQ 登录，不返回任何发件凭据。
#[tauri::command]
pub async fn get_manual_exclusions(
    state: State<'_, Arc<AppState>>,
) -> AppResult<ManualExclusionView> {
    let database = state.database()?;
    tokio::task::spawn_blocking(move || database.manual_exclusions())
        .await
        .map_err(|_| AppError::new("manual_exclusion_read", "手动排除读取中断，请刷新重试。"))?
}

/// 旧邮件草稿按原快照版本重新派生，允许已退出 QQ 后查看；返回版本供前端拒绝迟到响应。
#[tauri::command]
pub async fn get_snapshot_view(
    state: State<'_, Arc<AppState>>,
    snapshot_id: String,
) -> AppResult<Snapshot> {
    let _operation = state.operation.lock().await;
    let database = state.database()?;
    tokio::task::spawn_blocking(move || snapshot_view_for_export(&database, &snapshot_id, None))
        .await
        .map_err(|_| AppError::new("snapshot_read", "名单读取中断，请重试。"))?
}

/// 恢复本版本指定成员；宿主锁禁止与获取、全局排除和发送交叉，不需要 QQ 登录。
/// 页面只提交号码与已读规则版本；实际成员和原过滤原因必须来自数据库，不能由前端构造。
#[tauri::command]
pub async fn restore_filtered_members(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    snapshot_id: String,
    qq_numbers: Vec<String>,
    expected_revision: u64,
) -> AppResult<Snapshot> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let original_id = snapshot_id.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        database.restore_filtered_members(&snapshot_id, &qq_numbers, expected_revision)
    })
    .await
    .map_err(|_| {
        AppError::new(
            "snapshot_restoration",
            "名单恢复中断，请重新加载此群名单后核对。",
        )
    })??;
    // 原草稿仍可使用原不可变版本，但其发送确认必须重算；新版本由页面明确交接后才进入邮件。
    state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    {
        let mut inner = state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner
            .view
            .snapshot
            .as_ref()
            .is_some_and(|value| value.id == original_id)
        {
            inner.view.snapshot = Some(snapshot.clone());
        }
    }
    publish(&app, &state);
    Ok(snapshot)
}

/// 写入成功后先废弃已确认范围，再刷新当前原始快照的派生结果；刷新失败也不能复用旧确认。
async fn complete_mutation(
    app: &AppHandle,
    state: &AppState,
    mutation: ManualExclusionMutation,
) -> AppResult<ManualExclusionMutation> {
    state
        .prepared_recipients
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Err(error) = refresh_current_overlay(state).await {
        // 数据库已经提交，不能向页面伪装成旧版本。保留旧名单供用户辨认，但广播新版本，
        // 使界面知道名单需要重取；旧确认已清除，导出/发送仍由后端校验防止沿用旧范围。
        state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .view
            .exclusion_revision = mutation.view.revision;
        publish(app, state);
        diagnostics::event(
            "warn",
            "manual_exclusions",
            "saved_refresh_failed",
            serde_json::json!({"revision": mutation.view.revision, "error_code": error.code}),
        );
        return Err(AppError::new(
            "manual_exclusion_refresh",
            "手动排除已保存，但当前名单刷新失败，请刷新后重试。",
        ));
    }
    publish(app, state);
    diagnostics::event(
        "info",
        "manual_exclusions",
        "updated",
        serde_json::json!({
            "revision": mutation.view.revision, "count": mutation.view.records.len(), "added": mutation.added,
            "skipped": mutation.skipped, "invalid": mutation.invalid, "removed": mutation.removed,
        }),
    );
    Ok(mutation)
}

/// QQ 号或完整邮箱由核心层统一解析；运行期间禁止写入，避免已确认发送范围在执行中变化。
#[tauri::command]
pub async fn add_manual_exclusions(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    text: String,
    note: String,
) -> AppResult<ManualExclusionMutation> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let mutation =
        tokio::task::spawn_blocking(move || database.add_manual_exclusions(&text, &note))
            .await
            .map_err(|_| {
                AppError::new("manual_exclusion_write", "手动排除保存中断，请刷新后重试。")
            })??;
    complete_mutation(&app, &state, mutation).await
}

/// 仅删除明确选择的本机记录；空值、非法 id 和事务校验由核心层统一处理，不能按邮箱泛删。
#[tauri::command]
pub async fn remove_manual_exclusions(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    ids: Vec<i64>,
) -> AppResult<ManualExclusionMutation> {
    let _operation = state.operation.lock().await;
    state.ensure_idle()?;
    let database = state.database()?;
    let mutation = tokio::task::spawn_blocking(move || database.remove_manual_exclusions(&ids))
        .await
        .map_err(|_| {
            AppError::new("manual_exclusion_write", "手动排除删除中断，请刷新后重试。")
        })??;
    complete_mutation(&app, &state, mutation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use member_core::verify;
    use std::path::PathBuf;

    /// 每项回归只创建随机临时目录和合成名单；不读取程序数据目录或联网登录。
    struct Fixture {
        database: Database,
        folder: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let folder = std::env::temp_dir().join(format!(
                "qq-manual-exclusion-host-test-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&folder).unwrap();
            Self {
                database: Database::open(folder.join("data.sqlite3")).unwrap(),
                folder,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // 只清理本测试已知的三个 SQLite 文件和空目录，不递归删除任何路径。
            for file in ["data.sqlite3", "data.sqlite3-wal", "data.sqlite3-shm"] {
                let _ = std::fs::remove_file(self.folder.join(file));
            }
            let _ = std::fs::remove_dir(&self.folder);
        }
    }

    /// 快照时间和号码均为固定合成数据；第二个版本用于证明不会回退到另一个群的最新名单。
    fn snapshot(id: &str, group_id: &str, finished_at: &str) -> Snapshot {
        verify::create_snapshot(
            id.into(),
            Account {
                qq_number: "990000".into(),
                nickname: "合成账号".into(),
                source: "web".into(),
            },
            Group {
                id: group_id.into(),
                name: "合成群".into(),
                member_count: Some(2),
                role: "member".into(),
            },
            FetchResult {
                members: ["990101", "990102"]
                    .into_iter()
                    .map(|qq_number| Member {
                        qq_number: qq_number.into(),
                        nickname: "合成成员".into(),
                        card: String::new(),
                        friend_relation: FriendRelation::Unknown,
                    })
                    .collect(),
                reported_count: Some(2),
                freshness_confirmed: true,
                truncated: false,
                warnings: vec![],
            },
            "2026-11-06T00:00:00Z".into(),
            finished_at.into(),
        )
    }

    #[test]
    fn fixed_snapshot_overlay_preserves_original_and_never_switches_to_the_latest_group() {
        let fixture = Fixture::new();
        let database = &fixture.database;
        let original = snapshot("older-version", "990001", "2026-11-06T00:01:00Z");
        database.save_snapshot(&original).unwrap();
        database
            .save_snapshot(&snapshot(
                "latest-version",
                "990002",
                "2026-11-06T00:02:00Z",
            ))
            .unwrap();
        let mutation = database
            .add_manual_exclusions("990101", "合成备注")
            .unwrap();
        let derived = snapshot_view_for_export(database, &original.id, None).unwrap();
        assert_eq!(derived.id, original.id);
        assert_eq!(derived.group.id, "990001");
        assert_eq!(derived.exclusion_revision, 1);
        assert_eq!(derived.members.len(), 1);
        assert_eq!(derived.members[0].qq_number, "990102");
        assert_eq!(derived.manual_filtered_count, 1);
        assert_eq!(derived.source_count, 2);

        let persisted = database.snapshot_by_id(&original.id).unwrap().unwrap();
        assert_eq!(persisted.members.len(), 2);
        assert_eq!(persisted.manual_filtered_count, 0);
        assert_eq!(persisted.exclusion_revision, 0);
        database
            .remove_manual_exclusions(&[mutation.view.records[0].id])
            .unwrap();
        let restored = snapshot_view_for_export(database, &original.id, Some(2)).unwrap();
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.members, original.members);
        assert_eq!(restored.manual_filtered_count, 0);
        assert_eq!(
            database.latest_snapshot().unwrap().unwrap().id,
            "latest-version"
        );
        assert_eq!(database.sent_count().unwrap(), 0);
    }

    #[test]
    fn export_requires_current_revision_and_missing_id_never_falls_back() {
        let fixture = Fixture::new();
        let database = &fixture.database;
        database
            .save_snapshot(&snapshot("fixed-version", "990001", "2026-11-06T00:01:00Z"))
            .unwrap();
        assert!(snapshot_view_for_export(database, "fixed-version", Some(0)).is_ok());
        database.add_manual_exclusions("990101", "").unwrap();
        assert_eq!(
            snapshot_view_for_export(database, "fixed-version", Some(0))
                .unwrap_err()
                .code,
            "exclusion_revision_changed"
        );
        // 旧 IPC 不提供版本仍执行当前规则，不能借缺少可选字段绕过手动排除。
        let current = snapshot_view_for_export(database, "fixed-version", None).unwrap();
        assert_eq!((current.members.len(), current.exclusion_revision), (1, 1));
        assert!(snapshot_view_for_export(database, "fixed-version", Some(1)).is_ok());
        for (id, code) in [("", "no_result"), ("missing-version", "snapshot_missing")] {
            assert_eq!(
                snapshot_view_for_export(database, id, None)
                    .unwrap_err()
                    .code,
                code
            );
        }
    }

    #[tokio::test]
    async fn startup_and_refresh_rederive_the_same_id_without_qq_login() {
        let fixture = Fixture::new();
        let database = &fixture.database;
        let original = snapshot("current-version", "990001", "2026-11-06T00:01:00Z");
        database.save_snapshot(&original).unwrap();
        let mutation = database.add_manual_exclusions("990101", "").unwrap();
        let state = AppState::new();
        state.initialize_database(database.clone()).unwrap();
        assert!(state.view().account.is_none());
        assert_eq!(state.view().snapshot.as_ref().unwrap().members.len(), 1);
        assert_eq!(state.view().exclusion_revision, 1);

        // 即使有更晚的其他群历史，刷新也必须恢复当前 id 的原始成员，再叠加现行规则。
        database
            .save_snapshot(&snapshot(
                "newer-other-group",
                "990002",
                "2026-11-06T00:02:00Z",
            ))
            .unwrap();
        database
            .remove_manual_exclusions(&[mutation.view.records[0].id])
            .unwrap();
        let _operation = state.operation.lock().await;
        let refreshed = refresh_current_overlay(&state).await.unwrap();
        let snapshot = refreshed.snapshot.unwrap();
        assert_eq!(snapshot.id, original.id);
        assert_eq!(snapshot.members, original.members);
        assert_eq!(snapshot.manual_filtered_count, 0);
        assert_eq!(refreshed.exclusion_revision, 2);
        assert_eq!(snapshot.exclusion_revision, 2);
        assert!(refreshed.account.is_none());
    }

    #[test]
    fn mutation_gate_rejects_running_fetch_and_mail_tasks() {
        let state = AppState::new();
        state.inner.lock().unwrap().view.task.status = "running".into();
        assert_eq!(state.ensure_idle().unwrap_err().code, "task_running");
        state.inner.lock().unwrap().view.task.status = "completed".into();
        state.mail.lock().unwrap().status = "running".into();
        assert_eq!(state.ensure_idle().unwrap_err().code, "mail_running");
        state.mail.lock().unwrap().status = "completed".into();
        assert!(state.ensure_idle().is_ok());
    }
}
