/**
 * 全局手动排除的独立工作区。数据由宿主事务维护，不把排除冒充已发送，
 * 失败保留输入、已选项和旧列表供重试；旧版本仅供查看，刷新到当前排除版本后才允许维护。
 * @author ahui
 */
import WorkbenchLayout from "./WorkbenchLayout";
import WorkspaceHeader from "./WorkspaceHeader";
import { Plus, RefreshCw, Search, Trash2, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { call, errorText } from "../lib/bridge";
import { notify } from "../lib/notify";
import { displayTime, sortListByLabel } from "../lib/view";
import type { ManualExclusionResult, ManualExclusionsView } from "../types";

interface Props {
  active: boolean;
  revision: number;
  running: boolean;
  onChanged: (view: ManualExclusionsView) => void;
  onBusyChange: (busy: boolean) => void;
  /** 从受阻成员跳转时定位对应规则；独立递增 revision 允许重复定位同一地址。 */
  searchRequest?: { query: string; revision: number };
}

/** 一次维护固定ID范围；离开页面撤销未提交确认，跨页选择与输入保留。 */
export default function ManualExclusionsPage(props: Props) {
  const [view, setView] = useState<ManualExclusionsView>({
    revision: 0,
    records: [],
  });
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [confirmation, setConfirmation] = useState<number[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const reading = useRef(false);
  // 同一时刻只读一次；版本变化/重新进入时合并为一次补读，不能被正在读取的门闩吞掉。
  const refreshQueued = useRef(false);
  const loadedRevision = useRef(0);
  const wasActive = useRef(false);
  const mounted = useRef(true);
  const epoch = useRef(0);
  const pageSelection = useRef<HTMLInputElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const locatedSearchRevision = useRef(0);
  const callbacks = useRef(props);
  callbacks.current = props;
  const disabled =
    props.running ||
    busy ||
    loading ||
    !!error ||
    !props.active ||
    view.revision < props.revision;
  // 排序副本只服务列表定位；删除确认和跨页勾选继续绑定数据库 ID。
  const sortedRecords = useMemo(
    () =>
      sortListByLabel(
        view.records,
        (record) => record.qqNumber || record.email,
        (record) => record.id,
      ),
    [view.records],
  );
  const matched = sortedRecords.filter((record) =>
    `${record.qqNumber} ${record.email} ${record.note}`
      .toLocaleLowerCase()
      .includes(query.trim().toLocaleLowerCase()),
  );
  const pages = Math.max(1, Math.ceil(matched.length / 25));
  const current = Math.min(page, pages);
  const rows = matched.slice((current - 1) * 25, current * 25);
  const matchedSelectedCount = matched.filter((record) =>
    selected.has(record.id),
  ).length;
  const pageSelectedCount = rows.filter((record) =>
    selected.has(record.id),
  ).length;
  const allPageSelected = rows.length > 0 && pageSelectedCount === rows.length;
  const somePageSelected = pageSelectedCount > 0;

  // 移除末页记录时同步页码，随后添加或刷新不应突然跳回之前的空末页。
  useEffect(() => {
    setPage((current) => Math.min(current, pages));
  }, [pages]);

  useEffect(() => {
    if (
      !props.active ||
      !props.searchRequest ||
      props.searchRequest.revision === locatedSearchRevision.current
    )
      return;
    // 每次明确定位只应用一次；普通离页再返回不能覆盖用户随后编辑过的搜索词。
    locatedSearchRevision.current = props.searchRequest.revision;
    setQuery(props.searchRequest.query);
    setPage(1);
    searchInput.current?.focus();
  }, [props.active, props.searchRequest]);

  useEffect(() => {
    if (pageSelection.current)
      pageSelection.current.indeterminate =
        somePageSelected && !allPageSelected;
  }, [somePageSelected, allPageSelected]);

  /**
   * 读取不占用业务任务；离页/版本变化立即使旧响应失效，挂起时的请求合并为一次后续读取。
   * 宿主若仍返回低于已知版本的快照，保留旧内容但禁用维护并提供重试，不无限自动轮询。
   */
  async function refresh() {
    if (!mounted.current || !callbacks.current.active) return;
    refreshQueued.current = true;
    if (pending.current || reading.current) return;
    reading.current = true;
    setLoading(true);
    setError("");
    try {
      do {
        refreshQueued.current = false;
        const request = ++epoch.current;
        const requestedRevision = callbacks.current.revision;
        try {
          const next = await call<ManualExclusionsView>(
            "get_manual_exclusions",
          );
          if (
            !mounted.current ||
            request !== epoch.current ||
            !callbacks.current.active
          )
            continue;
          if (next.revision < callbacks.current.revision) {
            // render可先于effect更新props；仅为新出现的版本补读，宿主持续落后时停在可重试错误。
            if (callbacks.current.revision > requestedRevision)
              refreshQueued.current = true;
            else setError("手动排除名单尚未同步到最新版本，请刷新重试。");
            continue;
          }
          loadedRevision.current = next.revision;
          setView(next);
          const ids = new Set(next.records.map((record) => record.id));
          setSelected(
            (value) => new Set([...value].filter((id) => ids.has(id))),
          );
          setConfirmation(null);
        } catch (failure) {
          if (
            mounted.current &&
            request === epoch.current &&
            callbacks.current.active
          )
            setError(errorText(failure));
        }
      } while (
        mounted.current &&
        callbacks.current.active &&
        refreshQueued.current &&
        !pending.current
      );
    } finally {
      reading.current = false;
      if (mounted.current) setLoading(false);
    }
  }

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      refreshQueued.current = false;
      wasActive.current = false;
      ++epoch.current;
      if (pending.current) callbacks.current.onBusyChange(false);
    };
  }, []);
  useEffect(() => {
    const activated = props.active && !wasActive.current;
    wasActive.current = props.active;
    ++epoch.current;
    if (!props.active) {
      refreshQueued.current = false;
      setConfirmation(null);
    } else if (activated || props.revision > loadedRevision.current)
      void refresh();
    // 写成功已带回完整视图；onChanged回传同一revision不必再读一次。
  }, [props.active, props.revision]);
  useEffect(() => {
    if (props.running) setConfirmation(null);
  }, [props.running]);

  /** 成功使用完整宿主视图替换；同步门闩阻止重复提交，任何错误都不清空用户输入。 */
  async function mutate(
    command: "add_manual_exclusions" | "remove_manual_exclusions",
    args: Record<string, unknown>,
  ) {
    if (disabled || pending.current || reading.current) return;
    pending.current = true;
    setBusy(true);
    callbacks.current.onBusyChange(true);
    let refreshAfterFailure = false;
    try {
      const result = await call<ManualExclusionResult>(command, args);
      if (!mounted.current) return;
      loadedRevision.current = result.view.revision;
      // 完整写入结果已经包含期间出现的旧版本请求；只为仍高于结果版本的新变化保留补读。
      if (result.view.revision >= callbacks.current.revision)
        refreshQueued.current = false;
      setView(result.view);
      callbacks.current.onChanged(result.view);
      setConfirmation(null);
      const ids = new Set(result.view.records.map((record) => record.id));
      setSelected((value) => new Set([...value].filter((id) => ids.has(id))));
      if (command === "add_manual_exclusions") {
        if (!result.invalid) {
          setText("");
          setNote("");
        }
        notify({
          message: `已添加 ${result.added} 条，重复 ${result.skipped} 条，无效 ${result.invalid} 条`,
          tone: result.invalid ? "info" : "success",
        });
      } else
        notify({
          message: `已移除 ${result.removed} 条手动排除`,
          tone: "success",
        });
    } catch (failure) {
      // 宿主可能已提交规则，只在刷新成员名单时失败；保留原提示和输入，不能把旧列表当成未保存。
      refreshAfterFailure = true;
      if (mounted.current)
        notify({ message: errorText(failure), tone: "error" });
    } finally {
      pending.current = false;
      if (mounted.current) {
        setBusy(false);
        callbacks.current.onBusyChange(false);
      }
      // 写入期间的 revision 事件会被门闩挡住，必须解锁后补读；读失败仍由页面提供重试入口。
      if (
        mounted.current &&
        (refreshAfterFailure ||
          refreshQueued.current ||
          callbacks.current.revision > loadedRevision.current)
      )
        await refresh();
    }
  }

  /** 复选框、整行与键盘共用选择边界；行内按钮和拖选文字不改变跨页选择。 */
  function toggleRecord(id: number) {
    if (disabled || pending.current || reading.current) return;
    setConfirmation(null);
    setSelected((value) => {
      const next = new Set(value);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  /**
   * 当前结果和本页选择共享同一范围更新，取消不会抹掉其他搜索结果或其他页的已选项。
   * @param ids 来自当前列表的记录标识；空范围不修改选择或待确认内容。
   * @param checked true 选中指定范围，false 仅取消指定范围。
   */
  function selectScope(ids: number[], checked: boolean) {
    if (disabled || pending.current || reading.current || !ids.length) return;
    setConfirmation(null);
    setSelected((value) => {
      const next = new Set(value);
      for (const id of ids) {
        if (checked) next.add(id);
        else next.delete(id);
      }
      return next;
    });
  }

  return (
    <section
      className="manual-exclusions-page"
      aria-labelledby="manual-exclusions-heading"
      aria-busy={loading || busy}
    >
      <WorkspaceHeader
        className="history-head"
        headingId="manual-exclusions-heading"
        title="手动排除"
        actions={
          <button
            className="button"
            disabled={loading || busy}
            onClick={() => void refresh()}
          >
            <RefreshCw size={15} className={loading ? "spin" : undefined} />
            刷新
          </button>
        }
      />
      <WorkbenchLayout
        className="manual-workbench"
        sidebarLabel="新增排除规则"
        contentLabel="排除规则列表"
        sidebar={
          <section className="workbench-panel">
            <h3 className="workbench-panel-title">新增规则</h3>
            <div className="manual-exclusions-input">
              <label>
                QQ 号或完整邮箱
                <textarea
                  aria-label="手动排除账号"
                  value={text}
                  disabled={disabled}
                  onChange={(event) => setText(event.target.value)}
                  placeholder={"一行一个 QQ 号或完整邮箱"}
                  rows={3}
                />
              </label>
              <label>
                备注（可选）
                <input
                  aria-label="手动排除备注"
                  value={note}
                  maxLength={500}
                  disabled={disabled}
                  onChange={(event) => setNote(event.target.value)}
                />
              </label>
              <button
                className="button primary"
                disabled={disabled || !text.trim()}
                onClick={() =>
                  void mutate("add_manual_exclusions", { text, note })
                }
              >
                <Plus size={15} />
                添加手动排除
              </button>
            </div>
          </section>
        }
      >
        {props.running && (
          <p className="secondary" role="status">
            任务进行中，可查看名单，结束后可维护。
          </p>
        )}
        {error && (
          <div className="history-load-state error-text" role="alert">
            <p>{error}</p>
            <button
              className="button"
              disabled={loading}
              onClick={() => void refresh()}
            >
              重试
            </button>
          </div>
        )}
        <div className="manual-exclusions-tools">
          <div className="search-field">
            <Search size={15} />
            <input
              ref={searchInput}
              aria-label="搜索手动排除"
              value={query}
              placeholder="搜索账号或备注"
              onChange={(event) => {
                if (pending.current) return;
                setConfirmation(null);
                setQuery(event.target.value);
                setPage(1);
              }}
            />
            {query && (
              <button
                className="icon-button"
                aria-label="清除手动排除搜索"
                disabled={busy}
                onClick={() => {
                  if (pending.current) return;
                  setConfirmation(null);
                  setQuery("");
                  setPage(1);
                  searchInput.current?.focus();
                }}
              >
                <X size={14} aria-hidden="true" />
              </button>
            )}
          </div>
          <span role="status">
            共 {view.records.length} 条
            {query.trim() ? ` · 匹配 ${matched.length} 条` : ""} · 已选{" "}
            {selected.size} 条
            {query.trim() && selected.size > matchedSelectedCount
              ? `（含搜索范围外 ${selected.size - matchedSelectedCount} 条）`
              : ""}
          </span>
          <button
            className="button"
            disabled={disabled || !matched.length}
            aria-pressed={
              matched.length > 0 && matchedSelectedCount === matched.length
            }
            onClick={() =>
              selectScope(
                matched.map((record) => record.id),
                matchedSelectedCount !== matched.length,
              )
            }
          >
            {matched.length > 0 && matchedSelectedCount === matched.length
              ? "取消全选（当前结果）"
              : "全选当前结果"}
          </button>
          {selected.size > 0 && (
            <button
              className="button"
              disabled={disabled || !selected.size}
              onClick={() => selectScope([...selected], false)}
            >
              清空全部选择
            </button>
          )}
          {selected.size > 0 && (
            <button
              className="button"
              disabled={disabled || !selected.size}
              onClick={() => setConfirmation([...selected])}
            >
              <Trash2 size={15} />
              移除所选
            </button>
          )}
        </div>
        {confirmation && (
          <div
            className="resolution-confirm"
            role="group"
            aria-label="确认移除手动排除"
          >
            <p>
              确认移除 {confirmation.length}{" "}
              条？这些账号将重新成为可发送候选，不会立即发信。
            </p>
            <button
              className="button"
              disabled={busy}
              onClick={() => setConfirmation(null)}
            >
              取消
            </button>
            <button
              className="button danger"
              disabled={disabled}
              onClick={() =>
                void mutate("remove_manual_exclusions", { ids: confirmation })
              }
            >
              确认移除
            </button>
          </div>
        )}
        {loading && (
          <p role="status" className="secondary">
            正在读取手动排除…
          </p>
        )}
        <div className="table-container manual-exclusions-table">
          <table>
            <thead>
              <tr>
                <th>
                  <input
                    ref={pageSelection}
                    type="checkbox"
                    aria-label="选择本页手动排除"
                    title={allPageSelected ? "取消全选本页" : "全选本页"}
                    disabled={disabled || !rows.length}
                    checked={allPageSelected}
                    onChange={(event) =>
                      selectScope(
                        rows.map((record) => record.id),
                        event.target.checked,
                      )
                    }
                  />
                </th>
                <th>账号</th>
                <th>备注</th>
                <th>添加时间</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {rows.length ? (
                rows.map((record) => (
                  <tr
                    key={record.id}
                    tabIndex={disabled ? undefined : 0}
                    aria-selected={selected.has(record.id)}
                    data-selectable={!disabled || undefined}
                    className={
                      selected.has(record.id) ? "member-selected" : undefined
                    }
                    onClick={(event) => {
                      if (
                        (event.target as Element).closest(
                          "input,button,a,label,select,textarea",
                        )
                      )
                        return;
                      const selection = window.getSelection();
                      if (
                        selection &&
                        !selection.isCollapsed &&
                        selection.anchorNode &&
                        event.currentTarget.contains(selection.anchorNode)
                      )
                        return;
                      toggleRecord(record.id);
                    }}
                    onKeyDown={(event) => {
                      if (
                        event.target !== event.currentTarget ||
                        ![" ", "Enter"].includes(event.key)
                      )
                        return;
                      event.preventDefault();
                      if (event.repeat) return;
                      toggleRecord(record.id);
                    }}
                  >
                    <td>
                      <input
                        type="checkbox"
                        aria-label={`选择排除记录 ${record.id}`}
                        checked={selected.has(record.id)}
                        disabled={disabled}
                        onChange={() => toggleRecord(record.id)}
                      />
                    </td>
                    <td>
                      {record.email}
                      <small>{record.qqNumber || ""}</small>
                    </td>
                    <td>{record.note || "—"}</td>
                    <td>{displayTime(record.createdAt)}</td>
                    <td>
                      <button
                        className="text-button"
                        aria-label={`移除排除记录 ${record.id}`}
                        disabled={disabled}
                        onClick={() => setConfirmation([record.id])}
                      >
                        移除
                      </button>
                    </td>
                  </tr>
                ))
              ) : (
                <tr>
                  <td colSpan={5} className="empty-table">
                    <span>
                      {loading
                        ? "正在加载…"
                        : query
                          ? "没有匹配的记录"
                          : "暂无手动排除"}
                    </span>
                    {!loading && query && (
                      <button
                        className="button"
                        disabled={busy}
                        onClick={() => {
                          if (pending.current) return;
                          setConfirmation(null);
                          setQuery("");
                          setPage(1);
                          searchInput.current?.focus();
                        }}
                      >
                        清除搜索
                      </button>
                    )}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
        <div className="pagination">
          <span>
            {current} / {pages} 页
          </span>
          <div className="pagination-controls">
            <button
              className="button"
              disabled={busy || current <= 1}
              onClick={() => {
                if (pending.current) return;
                // 翻页继续保留勾选，但撤销不可见记录的旧确认，防止回来后沿用旧操作。
                setConfirmation(null);
                setPage(current - 1);
              }}
            >
              上一页
            </button>
            <button
              className="button"
              disabled={busy || current >= pages}
              onClick={() => {
                if (pending.current) return;
                setConfirmation(null);
                setPage(current + 1);
              }}
            >
              下一页
            </button>
          </div>
        </div>
      </WorkbenchLayout>
    </section>
  );
}
