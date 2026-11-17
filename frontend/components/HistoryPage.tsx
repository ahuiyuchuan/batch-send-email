/** 本机发送历史与待核实投递的维护；人工结论必须与服务器接受证明区分。@author ahui */
import WorkbenchLayout from "./WorkbenchLayout";
import WorkspaceHeader from "./WorkspaceHeader";
import {
  ArrowLeft,
  CircleAlert,
  Eye,
  EyeOff,
  History,
  RefreshCw,
  LoaderCircle,
  Search,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { displayTime, maskedNumber, sortListByLabel } from "../lib/view";
import type { SentHistory } from "../types";

interface Props {
  /** 是否为当前工作区；离开页面只清理未提交确认，保留搜索和分页。 */
  active: boolean;
  returnLabel: "返回成员提取" | "返回写邮件" | "返回手动排除" | "返回关于";
  error: string | null;
  history: SentHistory;
  loading: boolean;
  busy: boolean;
  navigationBusy: boolean;
  running: boolean;
  revealed: boolean;
  text: string;
  query: string;
  pendingDeleteId: number | null;
  onText: (value: string) => void;
  onQuery: (value: string) => void;
  onBack: () => void;
  onRefresh: () => void;
  onReveal: () => void;
  onImport: (file: boolean) => void;
  /** true 仅表示整个提交成功；失败时组件保留二次确认和已选范围，供用户重试。 */
  onResolve: (id: number, accepted: boolean) => Promise<boolean>;
  onResolveMany: (ids: number[], accepted: boolean) => Promise<boolean>;
  onDelete: (id: number) => void;
  onCancelDelete: () => void;
}

/** 兼容旧来源值，但所有人工导入或核实都明确保留人工性质。 */
const ORIGIN_LABELS: Record<string, string> = {
  manual: "人工记录",
  imported: "导入记录",
  manual_verified: "人工核实已发送",
  smtp_accepted: "SMTP 已接受",
  import: "导入记录",
  smtp: "SMTP 已接受",
};

/**
 * 只展示投递时记录的归属，不能用当前 SMTP 目录补齐旧历史或人工标记，避免误导核实对象。
 * @param record 宿主返回的可选发件账号快照；缺字段和空串均为未记录，部分缺失逐项标明。
 * @returns 名称与邮箱的纯文本节点；两项都没有时统一显示“未记录”，不创建任何外部链接。
 */
function senderAccountContent(record: {
  senderName?: string;
  senderAddress?: string;
}) {
  const name = record.senderName?.trim();
  const address = record.senderAddress?.trim();
  if (!name && !address) return <span className="empty-value">未记录</span>;
  return (
    <>
      <span className="history-sender-name">{name || "名称未记录"}</span>
      <span className="history-sender-address">{address || "邮箱未记录"}</span>
    </>
  );
}

/** 本机标记不因导出而自动添加；删除采用两次明确操作，避免点击行时误删。 */
export default function HistoryPage(props: Props) {
  const [page, setPage] = useState(1);
  const [unresolvedPage, setUnresolvedPage] = useState(1);
  const [unresolvedQuery, setUnresolvedQuery] = useState("");
  const unresolvedSearch = useRef<HTMLInputElement>(null);
  const historySearch = useRef<HTMLInputElement>(null);
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  const [submitting, setSubmitting] = useState(false);
  // 同步门闩覆盖 React 尚未更新 busy 的间隙；整批不能双击重复提交或被选择操作改写。
  const submissionPending = useRef(false);
  const pageSelection = useRef<HTMLInputElement>(null);
  // 确认同时固定范围、结论和入口，不能提交后来改变的勾选，也不把批量拆成逐条命令。
  const [resolution, setResolution] = useState<{
    ids: number[];
    accepted: boolean;
    bulk: boolean;
  } | null>(null);
  const unresolved = props.history.unresolved;
  // 两类历史以收件账号定位，同账号按持久 ID 排列；维护操作始终提交原 ID。
  const sortedUnresolved = useMemo(
    () =>
      sortListByLabel(
        unresolved,
        (record) => record.email,
        (record) => record.id,
      ),
    [unresolved],
  );
  const sortedRecords = useMemo(
    () =>
      sortListByLabel(
        props.history.records,
        (record) => record.qqNumber || record.email,
        (record) => record.id,
      ),
    [props.history.records],
  );
  // 待核实和已发送分别搜索；只缩小展示/批量选择范围，不丢弃其他搜索条件下已勾选的记录。
  const matchedUnresolved = sortedUnresolved.filter((record) =>
    `${record.email} ${record.message} ${record.senderName ?? ""} ${record.senderAddress ?? ""}`
      .toLocaleLowerCase()
      .includes(unresolvedQuery.trim().toLocaleLowerCase()),
  );
  const matchedUnresolvedSelected = matchedUnresolved.filter((record) =>
    selectedIds.has(record.id),
  ).length;
  const unresolvedPages = Math.max(1, Math.ceil(matchedUnresolved.length / 25));
  const currentUnresolvedPage = Math.min(unresolvedPage, unresolvedPages);
  const unresolvedRows = matchedUnresolved.slice(
    (currentUnresolvedPage - 1) * 25,
    currentUnresolvedPage * 25,
  );
  const matched = sortedRecords.filter((record) =>
    `${record.qqNumber} ${record.email} ${record.note} ${record.senderName ?? ""} ${record.senderAddress ?? ""}`
      .toLocaleLowerCase()
      .includes(props.query.trim().toLocaleLowerCase()),
  );
  const pages = Math.max(1, Math.ceil(matched.length / 50));
  const current = Math.min(page, pages);
  const rows = matched.slice((current - 1) * 50, current * 50);
  const pendingDeleteRecord = props.history.records.find(
    (record) => record.id === props.pendingDeleteId,
  );
  // 刷新失败后保留旧内容供查阅，但先成功重载再进行维护，避免基于过时记录提交结论。
  const disabled =
    props.busy || props.running || props.loading || submitting || !!props.error;
  const allPageSelected =
    unresolvedRows.length > 0 &&
    unresolvedRows.every((record) => selectedIds.has(record.id));
  const somePageSelected = unresolvedRows.some((record) =>
    selectedIds.has(record.id),
  );

  // 删除/核实末页后持久收敛到有效页；后续刷新增加记录时不恢复失效的旧页码。
  useEffect(() => {
    setPage((current) => Math.min(current, pages));
  }, [pages]);
  useEffect(() => {
    setUnresolvedPage((current) => Math.min(current, unresolvedPages));
  }, [unresolvedPages]);

  useEffect(() => {
    const available = new Set(unresolved.map((record) => record.id));
    setSelectedIds((current) => {
      const next = new Set([...current].filter((id) => available.has(id)));
      return next.size === current.size ? current : next;
    });
    // 任何一个确认项消失就撤销整批确认，不能悄悄缩减用户已经核实的范围。
    setResolution((current) =>
      current?.ids.some((id) => !available.has(id)) ? null : current,
    );
  }, [unresolved]);

  useEffect(() => {
    if (pageSelection.current)
      pageSelection.current.indeterminate =
        somePageSelected && !allPageSelected;
  }, [somePageSelected, allPageSelected, props.loading]);

  // 三个主功能使用同级页面；离开时撤销未提交确认，避免回来后误沿用旧选择。
  useEffect(() => {
    if (!props.active) setResolution(null);
  }, [props.active]);

  /** 跨页保存选择；改变勾选会取消旧确认，避免按钮提交与当前看到的范围不一致。 */
  function changeSelection(update: (current: Set<number>) => Set<number>) {
    if (disabled || submissionPending.current) return;
    setResolution(null);
    setSelectedIds(update);
  }

  /** 复选框、整行和键盘只切换单条选择；所有入口仍受任务、读取和提交门闩约束。 */
  function toggleRecord(id: number) {
    changeSelection((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  /** 二次确认固定当前 ID 范围，列表顺序稳定且去重；此步骤不调用 IPC 或发信。 */
  function prepareResolution(ids: number[], accepted: boolean, bulk: boolean) {
    if (disabled || submissionPending.current || !ids.length) return;
    setResolution({ ids: [...new Set(ids)], accepted, bulk });
  }

  /** 失败保持确认和选择；成功以宿主返回历史为准剔除记录，绝不在本机预先标记为成功。 */
  async function submitResolution() {
    if (!resolution || disabled || submissionPending.current) return;
    const frozen = resolution;
    submissionPending.current = true;
    setSubmitting(true);
    try {
      const success = frozen.bulk
        ? await props.onResolveMany([...frozen.ids], frozen.accepted)
        : await props.onResolve(frozen.ids[0], frozen.accepted);
      if (success) setResolution(null);
    } finally {
      submissionPending.current = false;
      setSubmitting(false);
    }
  }

  /** 逐条和批量使用同一结论说明；人工“已发送”不能冒充 SMTP 证明，未发送不会触发重发。 */
  function resolutionConfirmation(bulk: boolean, email?: string) {
    if (!resolution) return null;
    return (
      <div
        className={`resolution-confirm${bulk ? " resolution-confirm-bulk" : ""}`}
        role="group"
        aria-label={bulk ? "批量核实投递" : `核实投递 ${email ?? ""}`}
      >
        <p>
          {resolution.accepted
            ? `确认将 ${resolution.ids.length} 条投递核实为已发送？将添加人工核实记录，不作为 SMTP 送达证明。`
            : `确认将 ${resolution.ids.length} 条投递核实为未发送？将解除这些地址的重发限制，不会立即发信；误判可能导致重复投递。`}
        </p>
        <div className="unresolved-actions">
          <button
            className={`button ${resolution.accepted ? "primary" : "danger"}`}
            disabled={disabled}
            onClick={() => void submitResolution()}
          >
            {submitting && (
              <LoaderCircle size={15} className="spin" aria-hidden="true" />
            )}
            {resolution.accepted ? "确认已发送" : "确认未发送"}
          </button>
          <button
            className="button"
            disabled={disabled}
            onClick={() => {
              if (!submissionPending.current) setResolution(null);
            }}
          >
            取消核实
          </button>
        </div>
      </div>
    );
  }

  return (
    <section
      className="history-page"
      aria-labelledby="history-heading"
      aria-busy={props.loading}
    >
      <WorkspaceHeader
        className="history-head"
        headingId="history-heading"
        title="已发送记录"
        meta={<>共 {props.history.total} 条</>}
        actions={
          <div className="history-heading-actions">
            <button
              className="button"
              onClick={() => {
                if (!submissionPending.current) {
                  setResolution(null);
                  props.onBack();
                }
              }}
              disabled={props.navigationBusy || submitting}
            >
              <ArrowLeft size={16} aria-hidden="true" />
              {props.returnLabel}
            </button>
            <button
              className="button history-refresh"
              disabled={props.busy || props.loading || submitting}
              onClick={() => {
                if (!submissionPending.current) {
                  setResolution(null);
                  props.onRefresh();
                }
              }}
            >
              <RefreshCw
                size={16}
                className={props.loading ? "spin" : ""}
                aria-hidden="true"
              />
              {props.loading ? "正在刷新…" : "刷新记录"}
            </button>
          </div>
        }
      />
      <div className="history-content">
        {props.error && (
          <div className="history-error" role="alert">
            <CircleAlert size={18} aria-hidden="true" />
            <div>
              <strong>记录操作未完成</strong>
              <p>{props.error}</p>
            </div>
            <button
              className="button"
              onClick={() => {
                if (!submissionPending.current) {
                  setResolution(null);
                  props.onRefresh();
                }
              }}
              disabled={props.busy || props.loading || submitting}
            >
              重新加载记录
            </button>
          </div>
        )}
        {props.running && (
          <p className="history-explainer">
            任务进行中，暂时无法修改发送记录或核实投递。
          </p>
        )}

        <WorkbenchLayout
          className="history-workbench"
          sidebarLabel="记录维护与投递核实"
          contentLabel="发送记录查询"
          sidebar={
            <>
              {!props.loading && unresolved.length > 0 && (
                <section
                  className="unresolved-section"
                  aria-labelledby="unresolved-heading"
                >
                  <h3 id="unresolved-heading">
                    <CircleAlert size={17} aria-hidden="true" />
                    待核实投递 {unresolved.length} 条
                  </h3>
                  {unresolved.length ? (
                    <p className="unresolved-description">
                      请先检查发件箱或服务端记录，再选择核实结果。这些地址在核实前不会重发。
                    </p>
                  ) : (
                    <p className="unresolved-description">没有待核实投递。</p>
                  )}
                  {unresolved.length > 0 && (
                    <>
                      <div className="search-field">
                        <Search size={15} aria-hidden="true" />
                        <input
                          ref={unresolvedSearch}
                          aria-label="搜索待核实投递"
                          placeholder="搜索待核实邮箱、提示或发件账号"
                          value={unresolvedQuery}
                          disabled={submitting}
                          onChange={(event) => {
                            if (submissionPending.current) return;
                            setResolution(null);
                            setUnresolvedQuery(event.target.value);
                            setUnresolvedPage(1);
                          }}
                        />
                        {unresolvedQuery && (
                          <button
                            className="icon-button"
                            aria-label="清除待核实搜索"
                            disabled={submitting}
                            onClick={() => {
                              if (submissionPending.current) return;
                              setResolution(null);
                              setUnresolvedQuery("");
                              setUnresolvedPage(1);
                              unresolvedSearch.current?.focus();
                            }}
                          >
                            <X size={14} aria-hidden="true" />
                          </button>
                        )}
                      </div>
                      <div
                        className="unresolved-toolbar"
                        aria-label="待核实批量操作"
                      >
                        <div className="unresolved-selection-actions">
                          <label className="unresolved-page-select">
                            <input
                              ref={pageSelection}
                              type="checkbox"
                              aria-label="本页全选待核实"
                              title="仅影响当前页，其他页的选择保留"
                              checked={allPageSelected}
                              disabled={disabled || !unresolvedRows.length}
                              onChange={() =>
                                changeSelection((current) => {
                                  const next = new Set(current);
                                  for (const record of unresolvedRows) {
                                    if (allPageSelected) next.delete(record.id);
                                    else next.add(record.id);
                                  }
                                  return next;
                                })
                              }
                            />
                            {allPageSelected ? "取消全选本页" : "全选本页"}
                          </label>
                          <button
                            className="button"
                            disabled={
                              disabled ||
                              !matchedUnresolved.length ||
                              matchedUnresolvedSelected ===
                                matchedUnresolved.length
                            }
                            onClick={() =>
                              changeSelection(
                                (current) =>
                                  new Set([
                                    ...current,
                                    ...matchedUnresolved.map(
                                      (record) => record.id,
                                    ),
                                  ]),
                              )
                            }
                          >
                            {unresolvedQuery.trim()
                              ? "全选待核实搜索结果"
                              : "全选全部待核实"}
                          </button>
                          {unresolvedQuery.trim() && (
                            <button
                              className="button"
                              disabled={disabled || !matchedUnresolvedSelected}
                              onClick={() =>
                                changeSelection((current) => {
                                  const next = new Set(current);
                                  for (const record of matchedUnresolved)
                                    next.delete(record.id);
                                  return next;
                                })
                              }
                            >
                              取消全选（待核实搜索结果）
                            </button>
                          )}
                          <button
                            className="button"
                            disabled={disabled || selectedIds.size === 0}
                            onClick={() => changeSelection(() => new Set())}
                          >
                            取消全选（全部待核实）
                          </button>
                          <span
                            className="unresolved-selected-count"
                            role="status"
                          >
                            已选 {selectedIds.size} 条
                            {unresolvedQuery.trim()
                              ? ` · 当前匹配 ${matchedUnresolved.length} 条，已选 ${matchedUnresolvedSelected} 条`
                              : ""}
                          </span>
                        </div>
                        <div className="unresolved-actions unresolved-bulk-actions">
                          <button
                            className="button"
                            aria-label="批量核实为已发送"
                            disabled={disabled || selectedIds.size === 0}
                            onClick={() =>
                              prepareResolution(
                                unresolved
                                  .filter((record) =>
                                    selectedIds.has(record.id),
                                  )
                                  .map((record) => record.id),
                                true,
                                true,
                              )
                            }
                          >
                            核实为已发送
                          </button>
                          <button
                            className="button"
                            aria-label="批量核实为未发送"
                            disabled={disabled || selectedIds.size === 0}
                            onClick={() =>
                              prepareResolution(
                                unresolved
                                  .filter((record) =>
                                    selectedIds.has(record.id),
                                  )
                                  .map((record) => record.id),
                                false,
                                true,
                              )
                            }
                          >
                            核实为未发送
                          </button>
                        </div>
                      </div>
                      {resolution?.bulk && resolutionConfirmation(true)}
                    </>
                  )}
                  <ul className="unresolved-list">
                    {unresolvedRows.map((record) => (
                      <li
                        className={`unresolved-row${selectedIds.has(record.id) ? " is-selected" : ""}`}
                        key={record.id}
                        tabIndex={disabled ? undefined : 0}
                        data-selectable={!disabled || undefined}
                        onClick={(event) => {
                          // 按钮和复选框各自处理操作；确认内容、文字拖选不能意外取消待提交范围。
                          if (
                            (event.target as Element).closest(
                              "input,button,a,label,select,textarea,[role='button'],.resolution-confirm",
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
                        <label className="unresolved-row-select">
                          <input
                            type="checkbox"
                            aria-label={`选择待核实 ${record.email}`}
                            checked={selectedIds.has(record.id)}
                            disabled={disabled}
                            onChange={() => toggleRecord(record.id)}
                          />
                        </label>
                        <div className="unresolved-details">
                          <strong className="unresolved-email">
                            {record.email}
                          </strong>
                          <small>{displayTime(record.updatedAt)}</small>
                          <dl className="unresolved-sender">
                            <dt>发件账号</dt>
                            <dd className="history-sender-account">
                              {senderAccountContent(record)}
                            </dd>
                          </dl>
                          <span>
                            {record.message || "投递结果不明确，请人工核实。"}
                          </span>
                        </div>
                        {!resolution?.bulk &&
                        resolution?.ids[0] === record.id ? (
                          resolutionConfirmation(false, record.email)
                        ) : (
                          <div className="unresolved-actions">
                            <button
                              className="button"
                              disabled={disabled}
                              aria-label={`核实为已发送 ${record.email}`}
                              onClick={() =>
                                prepareResolution([record.id], true, false)
                              }
                            >
                              核实为已发送
                            </button>
                            <button
                              className="button"
                              disabled={disabled}
                              aria-label={`核实为未发送 ${record.email}`}
                              onClick={() =>
                                prepareResolution([record.id], false, false)
                              }
                            >
                              核实为未发送
                            </button>
                          </div>
                        )}
                      </li>
                    ))}
                  </ul>
                  {unresolved.length > 0 && !unresolvedRows.length && (
                    <p className="secondary" role="status">
                      没有匹配的待核实投递，可清除搜索查看全部。
                    </p>
                  )}
                  {unresolvedPages > 1 && (
                    <div className="history-pagination">
                      <span className="secondary">
                        每页显示 25 条待核实记录
                      </span>
                      <button
                        className="button"
                        disabled={disabled || currentUnresolvedPage <= 1}
                        onClick={() => {
                          if (submissionPending.current) return;
                          setResolution(null);
                          setUnresolvedPage(currentUnresolvedPage - 1);
                        }}
                      >
                        上一页待核实
                      </button>
                      <span>
                        {currentUnresolvedPage} / {unresolvedPages}
                      </span>
                      <button
                        className="button"
                        disabled={
                          disabled || currentUnresolvedPage >= unresolvedPages
                        }
                        onClick={() => {
                          if (submissionPending.current) return;
                          setResolution(null);
                          setUnresolvedPage(currentUnresolvedPage + 1);
                        }}
                      >
                        下一页待核实
                      </button>
                    </div>
                  )}
                </section>
              )}
              {/* 维护入口按需展开，列表保持靠前；原生details只隐藏表单，不卸载或清空未提交输入。 */}
              <details
                className="history-entry-card"
                open
                aria-labelledby="history-import-heading"
              >
                <summary id="history-import-heading">
                  添加 / 导入已发送标记
                </summary>

                <label className="history-text-label">
                  <textarea
                    aria-label="添加已发送标记"
                    value={props.text}
                    disabled={disabled}
                    onChange={(event) => props.onText(event.target.value)}
                    placeholder={
                      "每行一个 QQ 号或数字@qq.com\n例如：123456789@qq.com"
                    }
                    rows={3}
                  />
                </label>
                <div className="history-import-actions">
                  <button
                    className="button"
                    disabled={disabled}
                    onClick={() => {
                      if (!submissionPending.current) {
                        setResolution(null);
                        props.onImport(true);
                      }
                    }}
                  >
                    <Upload size={15} />
                    导入 TXT / CSV
                  </button>
                  <button
                    className="button primary"
                    disabled={disabled || !props.text.trim()}
                    onClick={() => {
                      if (!submissionPending.current) {
                        setResolution(null);
                        props.onImport(false);
                      }
                    }}
                  >
                    添加记录
                  </button>
                </div>
              </details>
            </>
          }
        >
          <section
            className="history-records-card"
            aria-labelledby="history-list-heading"
          >
            <div className="history-list-head">
              <h3 id="history-list-heading">记录列表</h3>
              <button
                className="icon-button history-reveal"
                aria-label={
                  props.revealed ? "隐藏记录 QQ 号" : "显示记录完整 QQ 号"
                }
                aria-pressed={props.revealed}
                onClick={props.onReveal}
              >
                {props.revealed ? <Eye size={17} /> : <EyeOff size={17} />}
              </button>
              <div className="search-field">
                <Search size={15} />
                <input
                  ref={historySearch}
                  aria-label="搜索已发送记录"
                  value={props.query}
                  placeholder="搜索 QQ、邮箱、备注或发件账号"
                  onChange={(event) => {
                    if (submissionPending.current) return;
                    setResolution(null);
                    props.onCancelDelete();
                    props.onQuery(event.target.value);
                    setPage(1);
                  }}
                />
                {props.query && (
                  <button
                    className="icon-button"
                    aria-label="清除已发送搜索"
                    disabled={submitting}
                    onClick={() => {
                      if (submissionPending.current) return;
                      setResolution(null);
                      props.onCancelDelete();
                      props.onQuery("");
                      setPage(1);
                      historySearch.current?.focus();
                    }}
                  >
                    <X size={14} aria-hidden="true" />
                  </button>
                )}
              </div>
            </div>
            {props.pendingDeleteId !== null && (
              <p className="history-delete-warning" role="status">
                {pendingDeleteRecord && (
                  <>
                    将删除记录“
                    {pendingDeleteRecord.qqNumber
                      ? maskedNumber(
                          pendingDeleteRecord.qqNumber,
                          props.revealed,
                        )
                      : pendingDeleteRecord.email}
                    ”。
                  </>
                )}
                删除只影响本机过滤依据，不会撤回邮件。删除后该地址可能再次进入收件范围，请核对后确认。
              </p>
            )}
            {props.loading && props.history.records.length === 0 ? (
              <div className="history-empty history-loading" role="status">
                <LoaderCircle className="spin" size={24} />
                正在加载记录…
              </div>
            ) : rows.length ? (
              <div
                className="history-table-wrap"
                role="region"
                aria-label="已发送记录明细，可左右滚动查看"
                tabIndex={0}
              >
                {/* 窄窗仍可用键盘或触摸查看完整账号与操作，不让额外列推宽整个页面。 */}
                <p className="history-scroll-hint">
                  左右滚动可查看发件账号和操作。
                </p>
                <table>
                  <thead>
                    <tr>
                      <th>QQ 号 / 邮箱</th>
                      <th>记录时间</th>
                      <th className="history-sender-column">发件账号</th>
                      <th>来源与说明</th>
                      <th className="history-delete-column">操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((record) => (
                      <tr key={record.id}>
                        <td>
                          {record.qqNumber
                            ? maskedNumber(record.qqNumber, props.revealed)
                            : record.email}
                        </td>
                        <td>{displayTime(record.sentAt)}</td>
                        <td className="history-sender-account">
                          {senderAccountContent(record)}
                        </td>
                        <td>
                          {ORIGIN_LABELS[record.origin] || record.origin}
                          <small className="history-note">
                            {record.note || "—"}
                          </small>
                        </td>
                        <td>
                          {props.pendingDeleteId === record.id ? (
                            <div
                              className="delete-confirm"
                              role="group"
                              aria-label={`删除记录 ${record.qqNumber ? maskedNumber(record.qqNumber, props.revealed) : record.email}`}
                            >
                              <button
                                className="button danger"
                                disabled={disabled}
                                onClick={() => {
                                  if (!submissionPending.current) {
                                    setResolution(null);
                                    props.onDelete(record.id);
                                  }
                                }}
                              >
                                确认删除
                              </button>
                              <button
                                className="icon-button"
                                aria-label="取消删除记录"
                                disabled={disabled}
                                onClick={props.onCancelDelete}
                              >
                                <X size={14} />
                              </button>
                            </div>
                          ) : (
                            <button
                              className="icon-button"
                              disabled={disabled}
                              aria-label={`删除发送记录 ${record.qqNumber ? maskedNumber(record.qqNumber, props.revealed) : record.email}`}
                              onClick={() => {
                                if (!submissionPending.current) {
                                  setResolution(null);
                                  props.onDelete(record.id);
                                }
                              }}
                            >
                              <Trash2 size={15} />
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : (
              <div className="history-empty">
                <History size={28} strokeWidth={1.5} aria-hidden="true" />
                <p>
                  {props.history.total
                    ? "没有匹配的记录。"
                    : props.error
                      ? "记录暂不可用，请重新加载。"
                      : "还没有已发送记录，可以在上方添加或导入。"}
                </p>
                {props.history.total > 0 && props.query && (
                  <button
                    className="button"
                    onClick={() => {
                      if (submissionPending.current) return;
                      setResolution(null);
                      props.onCancelDelete();
                      props.onQuery("");
                      setPage(1);
                      historySearch.current?.focus();
                    }}
                  >
                    清除搜索
                  </button>
                )}
              </div>
            )}
            <div className="history-pagination">
              <span className="secondary">匹配 {matched.length} 条</span>
              <button
                className="button"
                disabled={disabled || current <= 1}
                onClick={() => {
                  if (!submissionPending.current) {
                    setResolution(null);
                    props.onCancelDelete();
                    setPage(current - 1);
                  }
                }}
              >
                上一页
              </button>
              <span>
                {current} / {pages}
              </span>
              <button
                className="button"
                disabled={disabled || current >= pages}
                onClick={() => {
                  if (!submissionPending.current) {
                    setResolution(null);
                    props.onCancelDelete();
                    setPage(current + 1);
                  }
                }}
              >
                下一页
              </button>
            </div>
          </section>
        </WorkbenchLayout>
      </div>
    </section>
  );
}
