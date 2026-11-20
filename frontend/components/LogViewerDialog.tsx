/**
 * 当前日志的轻量只读查看器。只调用固定无路径 IPC，不修改邮件草稿、运行任务或日志内容，
 * 不自动轮询；关闭后忽略迟到响应。日志按纯文本显示，外部内容不能生成 HTML 或执行代码。
 * @author ahui
 */
import DialogLayout from "./DialogLayout";
import { FolderOpen, LoaderCircle, RefreshCw, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { call, errorText, type RecentLogs } from "../lib/bridge";
import { notify } from "../lib/notify";

/** 由导航显式打开；Esc、关闭按钮只关闭查看器，不取消或变更任何业务任务。 */
export default function LogViewerDialog({ onClose }: { onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const content = useRef<HTMLTextAreaElement>(null);
  const mounted = useRef(false);
  const reading = useRef(false);
  const opening = useRef(false);
  const [logs, setLogs] = useState<RecentLogs | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [openingDirectory, setOpeningDirectory] = useState(false);

  /** 先退出原生模态层再卸载，让全局通知容器及时移回页面，不留在已移除的弹层中。 */
  function closeViewer() {
    dialog.current?.close();
    onClose();
  }

  /** 失败保留上次已读内容；同步门闩阻止 React 渲染前的重复刷新，不产生自动重试。 */
  async function readLogs() {
    if (reading.current) return;
    reading.current = true;
    setLoading(true);
    setError("");
    try {
      const value = await call<RecentLogs>("get_recent_logs");
      if (mounted.current) setLogs(value);
    } catch (failure) {
      if (mounted.current) setError(errorText(failure));
    } finally {
      reading.current = false;
      if (mounted.current) setLoading(false);
    }
  }

  /** 打开宿主固定目录，不向后台提交日志文本或页面提供的路径；失败只影响本次打开动作。 */
  async function openDirectory() {
    if (opening.current) return;
    opening.current = true;
    setOpeningDirectory(true);
    try {
      await call<void>("open_log_directory");
    } catch (failure) {
      if (mounted.current)
        notify({ message: errorText(failure), tone: "error" });
    } finally {
      opening.current = false;
      if (mounted.current) setOpeningDirectory(false);
    }
  }

  useEffect(() => {
    mounted.current = true;
    dialog.current?.showModal();
    void readLogs();
    return () => {
      mounted.current = false;
      dialog.current?.close();
    };
  }, []);

  return (
    <dialog
      ref={dialog}
      className="log-viewer-dialog dialog-shell"
      aria-labelledby="log-viewer-heading"
      onCancel={(event) => {
        event.preventDefault();
        closeViewer();
      }}
    >
      <DialogLayout
        heading={
          <header className="dialog-head">
            <h2 id="log-viewer-heading">查看日志</h2>
            <button
              className="icon-button"
              aria-label="关闭日志"
              onClick={closeViewer}
            >
              <X size={18} />
            </button>
          </header>
        }
        footer={
          <footer className="log-viewer-actions dialog-footer">
            <button
              className="button"
              disabled={!logs?.content}
              onClick={() => {
                // 只选中当前已显示的只读内容，便于复制；不申请剪贴板权限或读取未显示的日志。
                content.current?.focus();
                content.current?.select();
              }}
            >
              全选日志
            </button>
            <button
              className="button"
              disabled={loading}
              onClick={() => void readLogs()}
            >
              {loading ? (
                <LoaderCircle size={15} className="spin" />
              ) : (
                <RefreshCw size={15} />
              )}
              {loading ? "读取中" : "刷新"}
            </button>
            <button
              className="button"
              disabled={openingDirectory}
              onClick={() => void openDirectory()}
            >
              <FolderOpen size={15} />
              打开日志目录
            </button>
            <button className="button primary" onClick={closeViewer}>
              关闭
            </button>
          </footer>
        }
      >
        {error && (
          <div className="log-viewer-error" role="alert">
            <p>{error}</p>
            <button
              className="button"
              disabled={loading}
              onClick={() => void readLogs()}
            >
              重试
            </button>
          </div>
        )}
        {logs?.content ? (
          <textarea
            ref={content}
            className="log-viewer-content"
            aria-label="最近日志"
            value={logs.content}
            readOnly
            spellCheck={false}
            wrap="off"
          />
        ) : !error ? (
          <div className="log-viewer-empty" role="status">
            {loading ? "正在读取日志…" : "暂无日志"}
          </div>
        ) : null}
        {logs?.truncated && (
          <p className="log-viewer-note">
            仅显示最近日志，完整记录可打开日志目录查看。
          </p>
        )}
      </DialogLayout>
    </dialog>
  );
}
