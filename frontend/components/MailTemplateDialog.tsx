/**
 * 邮件模板选择与管理。列表和预览只读取模板；应用/编辑须交由写信区明确确认，
 * 避免浏览模板时登记资源或覆盖当前草稿。所有异步命令复用写信区的同步门闩。
 * @author ahui
 */
import DialogLayout from "./DialogLayout";
import {
  Copy,
  Download,
  FilePlus2,
  FileText,
  LoaderCircle,
  Pencil,
  Search,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { call, errorText } from "../lib/bridge";
import { notify } from "../lib/notify";
import { sortListByLabel } from "../lib/view";
import TemplateImportDialog from "./TemplateImportDialog";
import type {
  MailTemplateContent,
  MailTemplateExportResult,
  MailTemplateImportPolicy,
  MailTemplateImportPreview,
  MailTemplateImportResult,
  MailTemplateSummary,
} from "../mail-types";

interface Props {
  open: boolean;
  busy: boolean;
  onClose: () => void;
  onNew: () => void;
  onUse: (value: MailTemplateContent, intent: "apply" | "edit") => void;
  onChanged: (value: MailTemplateSummary) => void;
  onDeleted: (id: string) => void;
  perform: <T>(operation: () => Promise<T>) => Promise<T>;
  previewDocument: (value: MailTemplateContent) => string;
}

type ManagementAction = "rename" | "duplicate" | "delete";

/** 模板时间显示完整本地日期时间；无法解析的历史值直接保留，不猜测时区。 */
function templateDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat("zh-CN", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
      }).format(date);
}

/** 模态窗口使用原生 dialog 保留焦点约束；操作失败只提示，不改变草稿或列表选择。 */
export default function MailTemplateDialog({
  open,
  busy,
  onClose,
  onNew,
  onUse,
  onChanged,
  onDeleted,
  perform,
  previewDocument,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const actionDialog = useRef<HTMLDialogElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const [templates, setTemplates] = useState<MailTemplateSummary[]>([]);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState("");
  const [preview, setPreview] = useState<MailTemplateContent | null>(null);
  const [document, setDocument] = useState("");
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [action, setAction] = useState<ManagementAction | null>(null);
  const [actionName, setActionName] = useState("");
  // 导出复选与单个预览独立维护；搜索只缩小范围，不把隐藏的已选模板悄悄丢弃。
  const [exportIds, setExportIds] = useState<Set<string>>(new Set());
  const [exchangeOperation, setExchangeOperation] = useState<
    "import" | "export" | null
  >(null);
  const [importContext, setImportContext] = useState<{
    value: MailTemplateImportPreview;
    expiresAt: number;
  } | null>(null);
  const [importError, setImportError] = useState("");
  const [importInvalid, setImportInvalid] = useState(false);
  // token 仅保留在组件内存；卸载时只释放当前预览，不能让另一批导入复用它。
  const importToken = useRef<string | null>(null);
  const pending = useRef(false);
  const epoch = useRef(0);
  const callbacks = useRef({ perform, previewDocument });
  callbacks.current = { perform, previewDocument };
  const selected = templates.find((item) => item.id === selectedId);
  const normalizedQuery = query.trim().toLocaleLowerCase();
  // 重命名后自动回到名称对应位置，预览及导出勾选仍通过模板 ID 保留。
  const sortedTemplates = useMemo(
    () =>
      sortListByLabel(
        templates,
        (item) => item.name,
        (item) => item.id,
      ),
    [templates],
  );
  // 一次读取轻量摘要后在本机匹配名称和主题，连续输入不触发并发 IPC 或不断打断焦点。
  const visible = sortedTemplates.filter((item) =>
    `${item.name}\n${item.subject}`
      .toLocaleLowerCase()
      .includes(normalizedQuery),
  );
  const visibleExportCount = visible.filter((item) =>
    exportIds.has(item.id),
  ).length;

  useEffect(() => {
    const available = new Set(templates.map((item) => item.id));
    setExportIds(
      (current) => new Set([...current].filter((id) => available.has(id))),
    );
  }, [templates]);

  useEffect(
    () => () => {
      const token = importToken.current;
      if (token)
        void call<void>("mail_template_import_cancel", { token }).catch(
          () => {},
        );
    },
    [],
  );

  /** 批量导出只改变复选集合，不选择预览、不登记草稿资源，也不清除搜索外的复选项。 */
  function changeExportSelection(ids: string[], checked: boolean) {
    if (busy || pending.current) return;
    setExportIds((current) => {
      const next = new Set(current);
      for (const id of ids) {
        if (checked) next.add(id);
        else next.delete(id);
      }
      return next;
    });
  }

  /** 固定本次标识后打开系统保存框；取消保留选择，成功或失败均不改动模板及当前邮件。 */
  async function exportTemplates(ids: string[]) {
    if (busy || pending.current || !ids.length) return;
    pending.current = true;
    setExchangeOperation("export");
    try {
      const result = await perform(() =>
        call<MailTemplateExportResult>("mail_template_export", {
          ids: [...new Set(ids)],
        }),
      );
      if (result.saved)
        notify({
          message: `已导出 ${result.count} 个模板（含正文图片及已保存附件）。`,
          tone: "success",
        });
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    } finally {
      pending.current = false;
      setExchangeOperation(null);
    }
  }

  /**
   * 系统选择器只返回校验后的轻量预览；重新选文件会使宿主原 token 失效。
   * 读取失败保留旧预览供核对，但禁止沿用失效 token，必须重新选文件。
   */
  async function previewImport() {
    if (busy || pending.current) return;
    pending.current = true;
    setExchangeOperation("import");
    setImportError("");
    if (importContext) setImportInvalid(true);
    importToken.current = null;
    try {
      const value = await perform(() =>
        call<MailTemplateImportPreview | null>("mail_template_import_preview"),
      );
      importToken.current = value?.token ?? null;
      setImportContext(
        value ? { value, expiresAt: Date.now() + 5 * 60 * 1000 } : null,
      );
      setImportInvalid(false);
    } catch (error) {
      setImportError(errorText(error));
      notify({ message: errorText(error), tone: "error" });
    } finally {
      pending.current = false;
      setExchangeOperation(null);
    }
  }

  /** Esc、取消与关闭都释放同一 token；释放失败保留窗口，过期/已释放可直接关闭。 */
  async function cancelImport() {
    if (busy || pending.current || !importContext) return;
    pending.current = true;
    setExchangeOperation("import");
    try {
      const token = importToken.current;
      if (token)
        await perform(() =>
          call<void>("mail_template_import_cancel", { token }),
        );
      importToken.current = null;
      setImportContext(null);
      setImportError("");
    } catch (error) {
      if (
        typeof error === "object" &&
        error &&
        "code" in error &&
        error.code === "template_import_expired"
      ) {
        importToken.current = null;
        setImportContext(null);
      } else {
        setImportError(errorText(error));
        notify({ message: errorText(error), tone: "error" });
      }
    } finally {
      pending.current = false;
      setExchangeOperation(null);
    }
  }

  /**
   * 导入由宿主一次事务提交；返回摘要合并至原库，已应用模板仅同步名称元数据。
   * 当前预览被替换时在同一门闩内重读，读取失败也清掉旧内容，防止应用过时预览。
   */
  async function commitImport(policy: MailTemplateImportPolicy) {
    if (busy || pending.current || !importContext || importInvalid) return;
    pending.current = true;
    setExchangeOperation("import");
    setImportError("");
    try {
      const { result, refreshed, refreshedDocument, previewFailure } =
        await perform(async () => {
          const result = await call<MailTemplateImportResult>(
            "mail_template_import_commit",
            { token: importContext.value.token, policy },
          );
          importToken.current = null;
          let refreshed: MailTemplateContent | null = null;
          let refreshedDocument = "";
          let previewFailure = "";
          if (result.templates.some((item) => item.id === selectedId)) {
            try {
              refreshed = await call<MailTemplateContent>(
                "mail_template_preview",
                { id: selectedId },
              );
              refreshedDocument = previewDocument(refreshed);
            } catch (error) {
              refreshed = null;
              previewFailure = errorText(error);
            }
          }
          return { result, refreshed, refreshedDocument, previewFailure };
        });
      const changed = new Map(result.templates.map((item) => [item.id, item]));
      setTemplates((current) => [
        ...result.templates,
        ...current.filter((item) => !changed.has(item.id)),
      ]);
      for (const item of result.templates) onChanged(item);
      if (changed.has(selectedId)) {
        setPreview(refreshed);
        setDocument(refreshedDocument);
      }
      setLoadError(previewFailure);
      setImportContext(null);
      notify({
        message: `模板导入完成：新增 ${result.added} 个，覆盖 ${result.replaced} 个，跳过 ${result.skipped} 个。`,
        tone: "success",
      });
    } catch (error) {
      if (
        typeof error === "object" &&
        error &&
        "code" in error &&
        [
          "template_import_expired",
          "template_import_stale",
          "template_import_token",
        ].includes(String(error.code))
      ) {
        importToken.current = null;
        setImportInvalid(true);
      }
      setImportError(errorText(error));
      notify({ message: errorText(error), tone: "error" });
    } finally {
      pending.current = false;
      setExchangeOperation(null);
    }
  }

  /** 只有最后一次预览能生效；关闭窗口后迟到的读取结果也不能重新显示内容。 */
  async function selectTemplate(item: MailTemplateSummary) {
    if (pending.current) return;
    const request = ++epoch.current;
    pending.current = true;
    setSelectedId(item.id);
    setPreview(null);
    setDocument("");
    setLoading(true);
    setLoadError("");
    try {
      const value = await callbacks.current.perform(() =>
        call<MailTemplateContent>("mail_template_preview", { id: item.id }),
      );
      const safeDocument = callbacks.current.previewDocument(value);
      if (request === epoch.current) {
        setPreview(value);
        setDocument(safeDocument);
      }
    } catch (error) {
      if (request === epoch.current) {
        setLoadError(errorText(error));
        notify({ message: errorText(error), tone: "error" });
      }
    } finally {
      pending.current = false;
      if (request === epoch.current) setLoading(false);
    }
  }

  /** 摘要与资源分开读取；查看列表不会占用草稿附件句柄。 */
  async function loadTemplates() {
    if (pending.current) return;
    const request = ++epoch.current;
    pending.current = true;
    setLoading(true);
    setLoadError("");
    setTemplates([]);
    setSelectedId("");
    setPreview(null);
    setDocument("");
    try {
      const values = await callbacks.current.perform(() =>
        call<MailTemplateSummary[]>("mail_template_list", { query: "" }),
      );
      if (request === epoch.current) {
        setTemplates(values);
        setSelectedId("");
      }
    } catch (error) {
      if (request === epoch.current) {
        setLoadError(errorText(error));
        notify({ message: errorText(error), tone: "error" });
      }
    } finally {
      pending.current = false;
      if (request === epoch.current) setLoading(false);
    }
  }

  useEffect(() => {
    if (open) {
      setQuery("");
      setExportIds(new Set());
      dialog.current?.showModal();
      void loadTemplates();
    } else {
      epoch.current += 1;
      const token = importToken.current;
      importToken.current = null;
      setImportContext(null);
      // 关闭整个模板库也应释放未提交的导入；卸载清理由独立 effect 兜底，避免遗留双层弹窗。
      if (token)
        void call<void>("mail_template_import_cancel", { token }).catch(
          () => {},
        );
      actionDialog.current?.close();
      dialog.current?.close();
    }
    return () => {
      epoch.current += 1;
    };
  }, [open]);

  /** 重命名/复制先收集名称；删除单独确认，均不触碰当前已应用内容。 */
  function openAction(value: ManagementAction) {
    if (busy || pending.current || !selected) return;
    setAction(value);
    setActionName(
      value === "duplicate"
        ? `${[...selected.name].slice(0, 75).join("")} 副本`
        : selected.name,
    );
    actionDialog.current?.showModal();
  }

  /** 使用返回的真实摘要更新列表；命令失败时保留原名称、预览和当前邮件。 */
  async function commitAction() {
    if (pending.current || busy || !selected || !action) return;
    const name = actionName.trim();
    if (action !== "delete" && (!name || [...name].length > 80)) {
      notify({ message: "模板名称请填写 1 到 80 个字符。", tone: "error" });
      return;
    }
    pending.current = true;
    try {
      if (action === "delete") {
        await perform(() =>
          call<void>("mail_template_delete", { id: selected.id }),
        );
        setTemplates((current) =>
          current.filter((item) => item.id !== selected.id),
        );
        setSelectedId("");
        setPreview(null);
        setDocument("");
        onDeleted(selected.id);
        notify({ message: "模板已删除。", tone: "success" });
      } else {
        const value = await perform(() =>
          call<MailTemplateSummary>(
            action === "rename"
              ? "mail_template_rename"
              : "mail_template_duplicate",
            { id: selected.id, name },
          ),
        );
        setTemplates((current) => [
          value,
          ...current.filter((item) => item.id !== value.id),
        ]);
        if (action === "rename") {
          setPreview((current) =>
            current?.id === value.id
              ? { ...current, name: value.name }
              : current,
          );
          onChanged(value);
        }
        notify({
          message: action === "rename" ? "模板已重命名。" : "模板已复制。",
          tone: "success",
        });
      }
      actionDialog.current?.close();
      setAction(null);
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    } finally {
      pending.current = false;
    }
  }

  return (
    <>
      <dialog
        ref={dialog}
        className="mail-dialog mail-template-dialog dialog-shell"
        aria-labelledby="mail-template-heading"
        onCancel={(event) => {
          if (busy || pending.current) event.preventDefault();
          else onClose();
        }}
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-template-heading">邮件模板</h2>
              <div className="mail-template-heading-actions">
                <button
                  className="button"
                  disabled={busy}
                  onClick={() => void previewImport()}
                >
                  <Upload size={16} aria-hidden="true" />
                  导入模板
                </button>
                <button className="button" disabled={busy} onClick={onNew}>
                  <FilePlus2 size={16} />
                  新建模板
                </button>
                <button
                  className="icon-button"
                  disabled={busy}
                  aria-label="关闭邮件模板"
                  onClick={onClose}
                >
                  <X size={18} />
                </button>
              </div>
            </div>
          }
          footer={
            <div className="mail-dialog-actions mail-template-footer dialog-footer">
              <button
                className="button"
                disabled={busy || !exportIds.size}
                onClick={() => void exportTemplates([...exportIds])}
              >
                {exchangeOperation === "export" ? (
                  <LoaderCircle size={16} className="spin" aria-hidden="true" />
                ) : (
                  <Download size={16} aria-hidden="true" />
                )}
                导出已选 {exportIds.size} 个模板
              </button>
              <button className="button" disabled={busy} onClick={onClose}>
                取消
              </button>
              <button
                className="button primary"
                disabled={busy || !preview}
                onClick={() => preview && onUse(preview, "apply")}
              >
                使用此模板
              </button>
            </div>
          }
        >
          <div className="mail-template-layout">
            <div className="mail-template-browser">
              <div className="mail-template-search">
                <Search size={16} aria-hidden="true" />
                <input
                  ref={searchInput}
                  aria-label="搜索模板名称或主题"
                  placeholder="搜索名称或主题"
                  value={query}
                  disabled={busy}
                  onChange={(event) => setQuery(event.target.value)}
                />
                {query && (
                  <button
                    className="icon-button"
                    disabled={busy}
                    aria-label="清除模板搜索"
                    onClick={() => {
                      setQuery("");
                      searchInput.current?.focus();
                    }}
                  >
                    <X size={15} aria-hidden="true" />
                  </button>
                )}
              </div>
              <div
                className="mail-template-export-tools"
                aria-label="模板导出选择"
              >
                <p className="mail-dialog-note" role="status">
                  {normalizedQuery
                    ? `匹配 ${visible.length} / 共 ${templates.length} 个模板`
                    : `共 ${templates.length} 个模板`}
                  {` · 已选导出 ${exportIds.size} 个`}
                  {exportIds.size > visibleExportCount
                    ? `（搜索外 ${exportIds.size - visibleExportCount} 个）`
                    : ""}
                </p>
                <div>
                  <button
                    className="mail-text-action"
                    disabled={busy || !visible.length}
                    aria-pressed={
                      visible.length > 0 &&
                      visibleExportCount === visible.length
                    }
                    onClick={() =>
                      changeExportSelection(
                        visible.map((item) => item.id),
                        visibleExportCount !== visible.length,
                      )
                    }
                  >
                    {visible.length > 0 && visibleExportCount === visible.length
                      ? "取消全选（当前结果）"
                      : "全选当前结果"}
                  </button>
                  {exportIds.size > 0 && (
                    <button
                      className="mail-text-action"
                      disabled={busy || !exportIds.size}
                      onClick={() =>
                        changeExportSelection([...exportIds], false)
                      }
                    >
                      清空导出选择
                    </button>
                  )}
                </div>
              </div>
              <div
                className="mail-template-list"
                aria-label="模板列表"
                aria-busy={loading}
              >
                {visible.map((item) => (
                  <div
                    key={item.id}
                    className="mail-template-row"
                    data-previewed={item.id === selectedId || undefined}
                  >
                    <label className="mail-template-export-check">
                      <input
                        type="checkbox"
                        aria-label={`选择导出模板 ${item.name}`}
                        checked={exportIds.has(item.id)}
                        disabled={busy}
                        onChange={(event) =>
                          changeExportSelection([item.id], event.target.checked)
                        }
                      />
                    </label>
                    <button
                      className={`mail-template-item${item.id === selectedId ? " selected" : ""}`}
                      disabled={busy}
                      aria-pressed={item.id === selectedId}
                      onClick={() => void selectTemplate(item)}
                    >
                      <FileText
                        size={24}
                        className="template-card-symbol"
                        aria-hidden="true"
                      />
                      {/* 卡片保留两行摘要；完整名称/主题可悬停或在右侧预览中读取，不截断存储内容。 */}
                      <strong title={item.name}>{item.name}</strong>
                      <span title={item.subject || "无主题"}>
                        {item.subject || "无主题"}
                      </span>
                      <small>
                        {item.inlineImageCount > 0 &&
                          `图片 ${item.inlineImageCount} · `}
                        {item.attachmentCount > 0 &&
                          `附件 ${item.attachmentCount} · `}
                        {templateDate(item.updatedAt)}
                      </small>
                    </button>
                  </div>
                ))}
                {!loading && !visible.length && (
                  <div className="mail-template-empty">
                    <span>
                      {loadError && templates.length === 0
                        ? "模板暂不可用"
                        : query
                          ? "没有匹配的模板"
                          : "还没有模板"}
                    </span>
                    {query && templates.length > 0 && (
                      <button
                        className="button"
                        disabled={busy}
                        onClick={() => {
                          setQuery("");
                          searchInput.current?.focus();
                        }}
                      >
                        清除搜索
                      </button>
                    )}
                  </div>
                )}
              </div>
            </div>
            <section className="mail-template-detail" aria-label="模板预览">
              {loading ? (
                <div className="mail-template-empty" role="status">
                  <LoaderCircle size={20} className="spin" />
                  正在读取模板…
                </div>
              ) : loadError ? (
                <div className="mail-template-empty">
                  <span>{loadError}</span>
                  <button
                    className="button"
                    onClick={() =>
                      selected
                        ? void selectTemplate(selected)
                        : void loadTemplates()
                    }
                  >
                    重试
                  </button>
                </div>
              ) : preview ? (
                <>
                  <div className="mail-template-preview-heading">
                    <h3>{preview.name}</h3>
                    <p>{preview.subject || "无主题"}</p>
                  </div>
                  <iframe
                    title="邮件模板正文预览"
                    sandbox=""
                    srcDoc={document}
                  />
                  {preview.attachments.length > 0 && (
                    <ul className="mail-template-files" aria-label="模板附件">
                      {preview.attachments.map((item) => (
                        <li key={item.id}>{item.name}</li>
                      ))}
                    </ul>
                  )}
                </>
              ) : (
                <div className="mail-template-empty">选择模板查看预览</div>
              )}
              {/* 即使图片损坏导致预览失败，也必须保留删除和重命名入口，避免产生无法管理的模板。 */}
              {selected && (
                <div className="mail-template-management">
                  <button
                    className="mail-text-action"
                    disabled={busy}
                    onClick={() => void exportTemplates([selected.id])}
                  >
                    <Download size={14} aria-hidden="true" />
                    导出当前模板
                  </button>
                  <button
                    className="mail-text-action"
                    disabled={busy || !preview}
                    onClick={() => preview && onUse(preview, "edit")}
                  >
                    <Pencil size={14} />
                    编辑模板
                  </button>
                  <button
                    className="mail-text-action"
                    disabled={busy}
                    onClick={() => openAction("rename")}
                  >
                    重命名模板
                  </button>
                  <button
                    className="mail-text-action"
                    disabled={busy}
                    onClick={() => openAction("duplicate")}
                  >
                    <Copy size={14} />
                    复制模板
                  </button>
                  <button
                    className="mail-text-action danger-text"
                    disabled={busy}
                    onClick={() => openAction("delete")}
                  >
                    <Trash2 size={14} />
                    删除模板
                  </button>
                </div>
              )}
            </section>
          </div>
        </DialogLayout>
      </dialog>
      {importContext && (
        <TemplateImportDialog
          preview={importContext.value}
          expiresAt={importContext.expiresAt}
          busy={busy || exchangeOperation !== null}
          invalid={importInvalid}
          error={importError}
          onCancel={() => void cancelImport()}
          onReselect={() => void previewImport()}
          onCommit={(policy) => void commitImport(policy)}
        />
      )}
      <dialog
        ref={actionDialog}
        className="mail-dialog mail-template-action-dialog dialog-shell"
        aria-labelledby="mail-template-action-heading"
        onCancel={(event) => {
          if (busy || pending.current) event.preventDefault();
        }}
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-template-action-heading">
                {action === "delete"
                  ? "删除模板"
                  : action === "duplicate"
                    ? "复制模板"
                    : "重命名模板"}
              </h2>
              <button
                className="icon-button"
                disabled={busy}
                aria-label="关闭模板操作"
                onClick={() => actionDialog.current?.close()}
              >
                <X size={18} />
              </button>
            </div>
          }
          footer={
            <div className="mail-dialog-actions dialog-footer">
              <button
                className="button"
                disabled={busy}
                onClick={() => actionDialog.current?.close()}
              >
                取消
              </button>
              <button
                className={`button ${action === "delete" ? "danger" : "primary"}`}
                disabled={busy}
                onClick={() => void commitAction()}
              >
                {action === "delete"
                  ? "确认删除"
                  : action === "duplicate"
                    ? "确认复制"
                    : "保存名称"}
              </button>
            </div>
          }
        >
          {action === "delete" ? (
            <p className="mail-dialog-note">
              确定删除“{selected?.name}”？删除后无法恢复，当前邮件不受影响。
            </p>
          ) : (
            <label className="mail-template-name-field">
              模板名称
              <input
                autoFocus
                aria-label="模板名称"
                maxLength={80}
                value={actionName}
                disabled={busy}
                onChange={(event) => setActionName(event.target.value)}
                onKeyDown={(event) => {
                  // 仅名称输入支持 Enter；删除始终保留显式按钮确认，输入法选词不触发提交。
                  if (event.key === "Enter" && !event.nativeEvent.isComposing) {
                    event.preventDefault();
                    void commitAction();
                  }
                }}
              />
            </label>
          )}
        </DialogLayout>
      </dialog>
    </>
  );
}
