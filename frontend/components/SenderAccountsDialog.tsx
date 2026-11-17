/**
 * 多个 SMTP 账号的本机维护窗口。目录顺序用于查找管理，凭据只显示保存状态；
 * 保存、清除、删除与重排均经父组件统一门闩，不能与预检或发送并行。
 * @author ahui
 */
import DialogLayout from "./DialogLayout";
import { ArrowDown, ArrowUp, Check, Plus, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { call, errorText } from "../lib/bridge";
import { notify } from "../lib/notify";
import type {
  MailSenderAccount,
  MailSenderAccounts,
  MailSettings,
} from "../mail-types";

interface Props {
  open: boolean;
  catalog: MailSenderAccounts;
  busy: boolean;
  loading: boolean;
  readError: boolean;
  onReload: () => void;
  onClose: () => void;
  onChanged: (value: MailSenderAccounts) => void;
  onEditingChange: (dirty: boolean) => void;
  perform: <T>(operation: () => Promise<T>) => Promise<T>;
}

/** 每次新建仅初始化公开默认值；密码不跨账号复制，也不写入 localStorage。 */
function accountSettings(account?: MailSenderAccount): MailSettings {
  return {
    host: account?.host ?? "smtp.qq.com",
    port: account?.port ?? 465,
    tlsMode: account?.tlsMode ?? "tls",
    username: account?.username ?? "",
    fromAddress: account?.fromAddress ?? "",
    fromName: account?.fromName ?? "",
    password: "",
  };
}

/** 名称为空时按邮箱保存；比较全部 SMTP 字段，包含仅修改服务器或端口的新建半成品。 */
function hasUnsavedSettings(
  account: MailSenderAccount | undefined,
  name: string,
  settings: MailSettings,
): boolean {
  return account
    ? (name.trim() || settings.fromAddress.trim()) !== account.name ||
        JSON.stringify(settings) !== JSON.stringify(accountSettings(account))
    : !!name.trim() ||
        JSON.stringify(settings) !== JSON.stringify(accountSettings());
}

/**
 * 已保存账号和当前输入分开维护，重排或失败不会覆盖编辑中的公开配置与授权码。
 * @param props 有序公开账号目录、统一异步门闩及成功更新回调，不包含已保存的凭据明文。
 * @returns 原生模态维护窗口；未保存输入只保留在当前页面内存，删除必须经过独立确认。
 */
export default function SenderAccountsDialog({
  open,
  catalog,
  busy,
  loading,
  readError,
  onReload,
  onClose,
  onChanged,
  onEditingChange,
  perform,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const deleteDialog = useRef<HTMLDialogElement>(null);
  const pending = useRef(false);
  const initialized = useRef(false);
  const [working, setWorking] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [settings, setSettings] = useState<MailSettings>(() =>
    accountSettings(),
  );
  const [error, setError] = useState("");
  const [deleting, setDeleting] = useState<MailSenderAccount | null>(null);
  // 切换和关闭窗口都保留未提交输入，避免误关丢失；密码仅在当前页面编辑内存中存在。
  const edits = useRef(
    new Map<string, { name: string; settings: MailSettings }>(),
  );
  const saved = catalog.accounts.find((item) => item.id === editingId);
  const savedPasswordAvailable =
    !!saved?.passwordSaved &&
    saved.host.trim().toLowerCase() === settings.host.trim().toLowerCase() &&
    saved.port === settings.port &&
    saved.tlsMode === settings.tlsMode &&
    saved.username === settings.username &&
    saved.fromAddress.trim() === settings.fromAddress.trim();
  const closeBlocked = busy || working || loading;
  const blocked = closeBlocked || readError;
  const dirty = hasUnsavedSettings(saved, name, settings);
  const anyDirty =
    dirty ||
    [...edits.current].some(
      ([id, draft]) =>
        id !== (editingId ?? "new") &&
        hasUnsavedSettings(
          catalog.accounts.find((account) => account.id === id),
          draft.name,
          draft.settings,
        ),
    );

  useEffect(() => onEditingChange(anyDirty), [anyDirty, onEditingChange]);

  useEffect(() => {
    if (!open) {
      dialog.current?.close();
      deleteDialog.current?.close();
      return;
    }
    if (!initialized.current) {
      const first = catalog.accounts[0];
      setEditingId(first?.id ?? null);
      setName(first?.name ?? "");
      setSettings(accountSettings(first));
      initialized.current = true;
    }
    setDeleting(null);
    dialog.current?.showModal();
    // 打开时才初始化；目录更新不得清空用户仍在编辑的字段。
  }, [open]);

  /** 切换表单只改变本窗口编辑上下文，不改变本次发送选择或草稿内容。 */
  function editAccount(account?: MailSenderAccount) {
    if (blocked) return;
    edits.current.set(editingId ?? "new", { name, settings });
    const draft = edits.current.get(account?.id ?? "new");
    setEditingId(account?.id ?? null);
    setName(draft?.name ?? account?.name ?? "");
    setSettings(draft?.settings ?? accountSettings(account));
    setError("");
  }

  /** 同步门闩先于 React 渲染生效；所有失败保留输入，成功才应用宿主返回的完整目录。 */
  async function mutate(operation: () => Promise<MailSenderAccounts>) {
    if (busy || pending.current) return null;
    pending.current = true;
    setWorking(true);
    setError("");
    try {
      const value = await perform(operation);
      onChanged(value);
      return value;
    } catch (cause) {
      setError(errorText(cause));
      notify({ message: errorText(cause), tone: "error" });
      const code =
        cause && typeof cause === "object" && "code" in cause ? cause.code : "";
      if (code === "sender_account_partial_remove") {
        // 宿主已提交删除，只是旧凭据清理失败。父组件已重新读取目录；本地也移除已删账号缓存，
        // 恢复其他账号或 new 半成品，不能让不可再编辑的已删 id 留下永久“未保存”状态。
        const id = deleting?.id;
        if (id) edits.current.delete(id);
        deleteDialog.current?.close();
        setDeleting(null);
        if (editingId === id) {
          const first = catalog.accounts.find((account) => account.id !== id);
          const draft = edits.current.get(first?.id ?? "new");
          setEditingId(first?.id ?? null);
          setName(draft?.name ?? first?.name ?? "");
          setSettings(draft?.settings ?? accountSettings(first));
        }
      }
      return null;
    } finally {
      pending.current = false;
      setWorking(false);
    }
  }

  /** 空授权码表示保留同一 SMTP 身份的原凭据；绑定发生变化时宿主仍独立校验。 */
  async function save() {
    if (blocked) return;
    if (
      !settings.host.trim() ||
      !settings.username.trim() ||
      !settings.fromAddress.trim()
    ) {
      setError("请填写 SMTP 服务器、登录账号和发件邮箱。");
      return;
    }
    const value = await mutate(() =>
      call<MailSenderAccounts>("mail_account_save", {
        input: {
          ...(editingId ? { id: editingId } : {}),
          name: name.trim() || settings.fromAddress.trim(),
          settings,
        },
      }),
    );
    if (!value) return;
    const account = value.accounts.find((item) =>
      editingId
        ? item.id === editingId
        : !catalog.accounts.some((old) => old.id === item.id),
    );
    edits.current.delete(editingId ?? "new");
    setEditingId(account?.id ?? null);
    setName(account?.name ?? "");
    setSettings(accountSettings(account));
    notify({
      message: account?.passwordSaved
        ? "发件设置已保存，下次可直接使用。"
        : "发件设置已保存，发送前请填写授权码并再次保存。",
      tone: "success",
    });
    onClose();
  }

  /** 只重排稳定 id；表单输入不随顺序变化，失败保持目录原顺序。 */
  async function move(id: string, offset: -1 | 1) {
    if (blocked) return;
    const ids = catalog.accounts.map((item) => item.id);
    const index = ids.indexOf(id);
    const destination = index + offset;
    if (index < 0 || destination < 0 || destination >= ids.length) return;
    [ids[index], ids[destination]] = [ids[destination], ids[index]];
    await mutate(() =>
      call<MailSenderAccounts>("mail_accounts_reorder", { ids }),
    );
  }

  /** 删除只影响账号及保存凭据，不会删除历史；失败保留确认窗口供核对或取消。 */
  async function remove() {
    if (!deleting || blocked) return;
    const id = deleting.id;
    const value = await mutate(() =>
      call<MailSenderAccounts>("mail_account_remove", { id }),
    );
    if (!value) return;
    edits.current.delete(id);
    deleteDialog.current?.close();
    setDeleting(null);
    if (editingId === id) {
      const first = value.accounts[0];
      // 删除当前账号后切换到仍存在的首项时，优先恢复它的未提交输入，不能用保存值覆盖缓存。
      const draft = edits.current.get(first?.id ?? "new");
      setEditingId(first?.id ?? null);
      setName(draft?.name ?? first?.name ?? "");
      setSettings(draft?.settings ?? accountSettings(first));
    }
  }

  return (
    <>
      <dialog
        ref={dialog}
        className="mail-dialog mail-accounts-dialog dialog-shell"
        aria-labelledby="mail-settings-heading"
        onCancel={(event) => {
          event.preventDefault();
          if (!closeBlocked && !pending.current) onClose();
        }}
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-settings-heading">发件设置</h2>
              <button
                className="icon-button"
                disabled={closeBlocked}
                aria-label="关闭发件设置"
                onClick={onClose}
              >
                <X size={18} aria-hidden="true" />
              </button>
            </div>
          }
          footer={
            <div className="mail-dialog-actions dialog-footer">
              {saved &&
                (saved.passwordSaved ||
                  saved.passwordStatus === "unavailable" ||
                  saved.passwordStatus === "mismatch") && (
                  <button
                    className="button"
                    disabled={blocked}
                    onClick={async () => {
                      const value = await mutate(() =>
                        call<MailSenderAccounts>(
                          "mail_account_clear_password",
                          {
                            id: saved.id,
                          },
                        ),
                      );
                      if (value) {
                        setSettings((previous) => ({
                          ...previous,
                          password: "",
                        }));
                        edits.current.delete(saved.id);
                        notify({
                          message: "已清除保存的授权码。",
                          tone: "success",
                        });
                      }
                    }}
                  >
                    清除已保存授权码
                  </button>
                )}
              <button
                className="button"
                disabled={closeBlocked}
                onClick={onClose}
              >
                取消
              </button>
              <button
                className="button primary"
                disabled={blocked}
                onClick={() => void save()}
              >
                <Check size={16} aria-hidden="true" />
                保存配置
              </button>
            </div>
          }
        >
          {readError && (
            <div className="mail-state-retry" role="alert">
              <span>发件账号暂不可用，请重新加载后继续。当前输入保留。</span>
              <button
                className="button"
                disabled={closeBlocked}
                onClick={onReload}
              >
                重新加载发件账号
              </button>
            </div>
          )}
          <div className="mail-account-management">
            <div className="mail-account-list-heading">
              <strong>发件账号（{catalog.accounts.length}/20）</strong>
              <button
                className="button"
                disabled={blocked || catalog.accounts.length >= 20}
                onClick={() => editAccount()}
              >
                <Plus size={15} aria-hidden="true" />
                添加账号
              </button>
            </div>

            <ul className="mail-account-list" aria-label="已保存发件账号">
              {catalog.accounts.map((account, index) => (
                <li
                  key={account.id}
                  aria-label={`发件账号 ${index + 1} ${account.name}`}
                  className={account.id === editingId ? "active" : ""}
                >
                  <button
                    className="mail-account-edit"
                    disabled={blocked}
                    aria-label={`编辑账号 ${account.fromAddress}`}
                    aria-pressed={account.id === editingId}
                    onClick={() => editAccount(account)}
                  >
                    <strong>
                      账号 {index + 1} · {account.name}
                    </strong>
                    <span>{account.fromAddress}</span>
                    <small
                      className={
                        account.passwordSaved ? "" : "mail-field-error"
                      }
                    >
                      {account.passwordSaved
                        ? "授权码已保存"
                        : account.passwordMessage || "尚未保存授权码"}
                    </small>
                  </button>
                  <div className="mail-account-row-actions">
                    <button
                      className="icon-button"
                      aria-label={`上移账号 ${account.fromAddress}`}
                      title="上移账号"
                      disabled={blocked || index === 0}
                      onClick={() => void move(account.id, -1)}
                    >
                      <ArrowUp size={16} aria-hidden="true" />
                    </button>
                    <button
                      className="icon-button"
                      aria-label={`下移账号 ${account.fromAddress}`}
                      title="下移账号"
                      disabled={
                        blocked || index === catalog.accounts.length - 1
                      }
                      onClick={() => void move(account.id, 1)}
                    >
                      <ArrowDown size={16} aria-hidden="true" />
                    </button>
                    <button
                      className="icon-button danger-text"
                      aria-label={`删除账号 ${account.fromAddress}`}
                      title="删除账号"
                      disabled={blocked}
                      onClick={() => {
                        setDeleting(account);
                        setError("");
                        deleteDialog.current?.showModal();
                      }}
                    >
                      <Trash2 size={16} aria-hidden="true" />
                    </button>
                  </div>
                </li>
              ))}
            </ul>
            {catalog.accounts.length === 0 && (
              <p className="mail-dialog-note">
                尚未保存账号，请在右侧填写配置。
              </p>
            )}
          </div>
          <div className="mail-account-form">
            <h3 className="mail-account-form-heading">
              {editingId ? "编辑账号" : "添加账号"}
            </h3>
            {anyDirty && (
              <p className="mail-dialog-note">
                有未保存的修改。请保存配置后再发送；关闭窗口后当前输入仍保留。
              </p>
            )}
            {anyDirty && (
              <button
                className="mail-text-action"
                disabled={blocked}
                onClick={() => {
                  edits.current.clear();
                  setName(saved?.name ?? "");
                  setSettings(accountSettings(saved));
                  setError("");
                }}
              >
                放弃未保存修改
              </button>
            )}
            <div className="mail-settings-grid">
              <label className="mail-full-field">
                账号名称（可选）
                <input
                  value={name}
                  maxLength={80}
                  disabled={blocked}
                  onChange={(event) => setName(event.target.value)}
                  placeholder="便于识别，留空使用发件邮箱"
                />
              </label>
              <label className="mail-full-field">
                SMTP 服务器
                <input
                  value={settings.host}
                  disabled={blocked}
                  onChange={(event) =>
                    setSettings({ ...settings, host: event.target.value })
                  }
                  placeholder="smtp.qq.com"
                />
              </label>
              <label>
                加密方式
                <select
                  value={settings.tlsMode}
                  disabled={blocked}
                  onChange={(event) => {
                    const mode = event.target.value as MailSettings["tlsMode"];
                    setSettings({
                      ...settings,
                      tlsMode: mode,
                      port: mode === "tls" ? 465 : 587,
                    });
                  }}
                >
                  <option value="tls">SSL/TLS</option>
                  <option value="starttls">STARTTLS</option>
                </select>
              </label>
              <label>
                端口
                <input
                  value={settings.port}
                  type="number"
                  min={1}
                  max={65535}
                  disabled={blocked}
                  onChange={(event) =>
                    setSettings({
                      ...settings,
                      port: Number(event.target.value),
                    })
                  }
                />
              </label>
              <label className="mail-full-field">
                SMTP 登录账号
                <input
                  value={settings.username}
                  disabled={blocked}
                  autoComplete="username"
                  onChange={(event) => {
                    const username = event.target.value;
                    setSettings({
                      ...settings,
                      username,
                      fromAddress:
                        !settings.fromAddress ||
                        settings.fromAddress === settings.username
                          ? username
                          : settings.fromAddress,
                    });
                  }}
                  placeholder="你的邮箱地址"
                />
              </label>
              <label className="mail-full-field">
                密码或授权码
                <input
                  value={settings.password}
                  type="password"
                  aria-label="密码或授权码"
                  aria-describedby={
                    !savedPasswordAvailable
                      ? "mail-saved-password-hint"
                      : undefined
                  }
                  title="请在邮箱中开启SMTP服务，并使用邮箱提供的授权码"
                  autoComplete="off"
                  disabled={blocked}
                  onChange={(event) =>
                    setSettings({ ...settings, password: event.target.value })
                  }
                  placeholder={
                    savedPasswordAvailable
                      ? "已保存授权码，留空继续使用"
                      : "输入邮箱授权码"
                  }
                />
                {!savedPasswordAvailable && (
                  <span
                    id="mail-saved-password-hint"
                    className={
                      saved &&
                      !savedPasswordAvailable &&
                      saved.passwordStatus !== "missing"
                        ? "mail-field-error"
                        : "mail-dialog-note"
                    }
                  >
                    {saved?.passwordSaved
                      ? "SMTP 身份已修改，请填写新授权码后保存。"
                      : saved?.passwordMessage || "请填写邮箱授权码。"}
                  </span>
                )}
              </label>
              <label className="mail-full-field">
                发件邮箱
                <input
                  value={settings.fromAddress}
                  type="email"
                  disabled={blocked}
                  onChange={(event) =>
                    setSettings({
                      ...settings,
                      fromAddress: event.target.value,
                    })
                  }
                />
              </label>
              <label className="mail-full-field">
                发件人名称（可选）
                <input
                  value={settings.fromName}
                  disabled={blocked}
                  onChange={(event) =>
                    setSettings({ ...settings, fromName: event.target.value })
                  }
                />
              </label>
            </div>
            {error && !deleting && (
              <p className="mail-field-error" role="alert">
                {error}
              </p>
            )}
          </div>
        </DialogLayout>
      </dialog>
      <dialog
        ref={deleteDialog}
        className="mail-dialog mail-account-delete-dialog dialog-shell"
        aria-labelledby="mail-account-delete-heading"
        onCancel={(event) => {
          event.preventDefault();
          if (!blocked) {
            deleteDialog.current?.close();
            setDeleting(null);
            setError("");
          }
        }}
      >
        <DialogLayout
          heading={
            <div className="mail-dialog-heading">
              <h2 id="mail-account-delete-heading">删除发件账号</h2>
            </div>
          }
          footer={
            <div className="mail-dialog-actions dialog-footer">
              <button
                className="button"
                disabled={blocked}
                onClick={() => {
                  deleteDialog.current?.close();
                  setDeleting(null);
                  setError("");
                }}
              >
                取消
              </button>
              <button
                className="button danger"
                disabled={blocked}
                onClick={() => void remove()}
              >
                确认删除账号
              </button>
            </div>
          }
        >
          <p className="mail-dialog-note">
            将删除“{deleting?.name}”（{deleting?.fromAddress}
            ）及其保存的授权码，已有投递记录继续保留。
          </p>
          {error && (
            <p className="mail-field-error" role="alert">
              {error}
            </p>
          )}
        </DialogLayout>
      </dialog>
    </>
  );
}
