/** 替换邮件的群成员范围前明确比较旧/新来源；正文、附件和补充邮箱不受此操作影响。@author ahui */
import DialogLayout from "./DialogLayout";
import { snapshotGroupLabel } from "../lib/group-selection";
import { useEffect, useRef } from "react";
import type { GroupMailAudience } from "../mail-types";

interface Props {
  previous: GroupMailAudience | null;
  next: GroupMailAudience | null;
  onCancel: () => void;
  onConfirm: () => void;
}

/** 弹层由待确认范围驱动，Esc 与取消一致，不在关闭时偷偷应用新名单。 */
export default function AudienceReplaceDialog({
  previous,
  next,
  onCancel,
  onConfirm,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (next && !dialog.current?.open) dialog.current?.showModal();
    if (!next && dialog.current?.open) dialog.current.close();
  }, [next]);
  return (
    <dialog
      ref={dialog}
      className="audience-replace-dialog dialog-shell"
      aria-labelledby="audience-replace-heading"
      onCancel={(event) => {
        event.preventDefault();
        onCancel();
      }}
    >
      <DialogLayout
        heading={<h2 id="audience-replace-heading">替换这封邮件的群成员？</h2>}
        footer={
          <div className="audience-replace-actions dialog-footer">
            <button className="button" autoFocus onClick={onCancel}>
              保留原收件范围
            </button>
            <button className="button primary" onClick={onConfirm}>
              替换群成员并写信
            </button>
          </div>
        }
      >
        <p>
          主题、正文、附件和补充邮箱会保留；群成员收件范围将按下面的新选择替换。
        </p>
        <dl>
          <div>
            <dt>原收件范围</dt>
            <dd>
              {previous?.snapshot.group.name || "未命名群"} ·{" "}
              {previous?.qqNumbers.length ?? 0} 人
              {previous && ` · ${snapshotGroupLabel(previous.snapshot)}`}
            </dd>
          </div>
          <div>
            <dt>新收件范围</dt>
            <dd>
              {next?.snapshot.group.name || "未命名群"} ·{" "}
              {next?.qqNumbers.length ?? 0} 人
              {next && ` · ${snapshotGroupLabel(next.snapshot)}`}
            </dd>
          </div>
        </dl>
        <p className="secondary">
          不会发送邮件。确认后仍需在写信页核对并确认发送。
        </p>
      </DialogLayout>
    </dialog>
  );
}
