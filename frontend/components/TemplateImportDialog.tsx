/**
 * 模板文件的只读预览与导入确认。资源字节、名称映射和事务交由宿主处理；
 * 这里只收集冲突策略，永不应用模板到草稿或触发邮件发送。
 * @author ahui
 */
import DialogLayout from "./DialogLayout";
import { LoaderCircle, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { sortListByLabel } from "../lib/view";
import type {
  MailTemplateImportPolicy,
  MailTemplateImportPreview,
} from "../mail-types";

interface Props {
  preview: MailTemplateImportPreview;
  /** 页面收到预览时的本地截止时间；宿主仍以自己的五分钟期限和模板库版本为准。 */
  expiresAt: number;
  busy: boolean;
  invalid: boolean;
  error: string;
  onCancel: () => void;
  onReselect: () => void;
  onCommit: (policy: MailTemplateImportPolicy) => void;
}

/**
 * 原生 dialog 保持焦点约束；取消与 Esc 共用释放入口，提交期间不能关闭或改变策略。
 * @param props 固定的轻量预览及父组件命令回调；失败保留预览和当前策略供重试。
 * @returns 导入确认窗口；只有确认按钮会提交策略，覆盖同名项须另行勾选明确后果。
 */
export default function TemplateImportDialog({
  preview,
  expiresAt,
  busy,
  invalid,
  error,
  onCancel,
  onReselect,
  onCommit,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [policy, setPolicy] = useState<MailTemplateImportPolicy>("copy");
  const [overwriteConfirmed, setOverwriteConfirmed] = useState(false);
  const [expired, setExpired] = useState(false);
  const mustReselect = invalid || expired;
  // 原包索引在本次 token 内固定，仅用于区分同名项；排序不能改变宿主的导入包或冲突策略。
  const sortedTemplates = useMemo(
    () =>
      sortListByLabel(
        preview.templates.map((item, packageIndex) => ({ item, packageIndex })),
        ({ item }) => item.name,
        ({ packageIndex }) => packageIndex,
      ),
    [preview.templates],
  );
  const images = preview.templates.reduce(
    (sum, item) => sum + item.inlineImageCount,
    0,
  );
  const attachments = preview.templates.reduce(
    (sum, item) => sum + item.attachmentCount,
    0,
  );
  const overwriting = policy === "overwrite" && preview.conflictCount > 0;

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  useEffect(() => {
    // 新文件始终回到安全默认策略，旧文件的覆盖确认不能沿用到另一批模板。
    setPolicy("copy");
    setOverwriteConfirmed(false);
    setExpired(Date.now() >= expiresAt);
    const timer = window.setTimeout(
      () => setExpired(true),
      Math.max(0, expiresAt - Date.now()),
    );
    return () => window.clearTimeout(timer);
  }, [preview.token, expiresAt]);

  return (
    <dialog
      ref={dialog}
      className="mail-dialog mail-template-import-dialog dialog-shell"
      aria-labelledby="mail-template-import-heading"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
    >
      <DialogLayout
        heading={
          <div className="mail-dialog-heading">
            <h2 id="mail-template-import-heading">导入邮件模板</h2>
            <button
              className="icon-button"
              disabled={busy}
              aria-label="关闭模板导入"
              onClick={onCancel}
            >
              <X size={18} aria-hidden="true" />
            </button>
          </div>
        }
        footer={
          <div className="mail-dialog-actions dialog-footer">
            <button className="button" disabled={busy} onClick={onCancel}>
              取消导入
            </button>
            <button className="button" disabled={busy} onClick={onReselect}>
              重新选择文件
            </button>
            <button
              className="button primary"
              disabled={
                busy ||
                mustReselect ||
                !preview.templates.length ||
                (overwriting && !overwriteConfirmed)
              }
              onClick={() => onCommit(policy)}
            >
              {busy && (
                <LoaderCircle size={16} className="spin" aria-hidden="true" />
              )}
              确认导入
            </button>
          </div>
        }
      >
        <p className="mail-dialog-note">
          共 {preview.templates.length} 个模板 · 图片 {images} 张 · 附件{" "}
          {attachments} 个 · 文件{" "}
          {(preview.totalBytes / 1024 / 1024).toFixed(2)} MiB
        </p>
        <p className="mail-dialog-note">
          与本机同名 {preview.conflictCount}{" "}
          个。导入只更新模板库，当前邮件内容和收件人保留。
        </p>
        <ul className="mail-template-import-list" aria-label="待导入模板">
          {sortedTemplates.map(({ item, packageIndex }) => (
            <li key={`${packageIndex}-${item.name}`}>
              <strong>{item.name}</strong>
              <span>{item.subject || "无主题"}</span>
              <small>
                图片 {item.inlineImageCount} 张 · 附件 {item.attachmentCount} 个
                {item.conflict ? " · 同名模板" : " · 新模板"}
              </small>
            </li>
          ))}
        </ul>
        <fieldset
          className="mail-template-import-policy"
          disabled={busy || mustReselect}
        >
          <legend>同名模板处理</legend>
          {(
            [
              {
                value: "copy",
                label: "另存为副本",
                hint: "保留现有模板；同名时自动使用新名称。",
              },
              {
                value: "skip",
                label: "跳过同名模板",
                hint: "保留现有模板，只导入没有同名的项。",
              },
              {
                value: "overwrite",
                label: "覆盖同名模板",
                hint: "用文件中的主题、正文、图片和附件替换同名模板。",
              },
            ] as const
          ).map((option) => (
            <label key={option.value}>
              <input
                type="radio"
                name="mail-template-import-policy"
                value={option.value}
                aria-label={option.label}
                checked={policy === option.value}
                disabled={
                  option.value === "overwrite" && !preview.overwriteAllowed
                }
                onChange={() => {
                  setPolicy(option.value);
                  setOverwriteConfirmed(false);
                }}
              />
              <span>
                <strong>{option.label}</strong>
                <small>{option.hint}</small>
              </span>
            </label>
          ))}
        </fieldset>
        {!preview.overwriteAllowed && (
          <p className="mail-dialog-note">
            存在同名歧义，无法安全覆盖。请选择另存副本或跳过同名模板。
          </p>
        )}
        {overwriting && (
          <label className="mail-template-overwrite-confirm">
            <input
              type="checkbox"
              disabled={busy || mustReselect}
              checked={overwriteConfirmed}
              onChange={(event) => setOverwriteConfirmed(event.target.checked)}
            />
            我已确认覆盖同名模板，原内容及资源将被替换
          </label>
        )}
        {(error || mustReselect) && (
          <div className="mail-field-error" role="alert">
            {error && <p>{error}</p>}
            {mustReselect && <p>预览已失效，请重新选择模板文件。</p>}
          </div>
        )}
      </DialogLayout>
    </dialog>
  );
}
