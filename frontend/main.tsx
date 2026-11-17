/** Desktop renderer entry. The UI has no direct HTTP or file-system access. @author ahui */
import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { reportFrontendFault } from "./lib/bridge";
import "./styles.css";
import "./workbench.css";

/** 错误只允许固定类型进入日志；不读取 message、cause、stack 或任意拒绝内容。 */
function faultType(reason: unknown): string {
  try {
    return reason &&
      typeof reason === "object" &&
      "name" in reason &&
      typeof reason.name === "string"
      ? reason.name
      : "unknown";
  } catch {
    return "unknown";
  }
}
// 完整地址只在本机提取包文件名，发往宿主前由 bridge 再次做文件名与错误类型白名单校验。
const reportError = (event: ErrorEvent) =>
  reportFrontendFault("error", {
    errorType: faultType(event.error),
    source: event.filename.split(/[\\/]/u).at(-1)?.split(/[?#]/u)[0],
    line: event.lineno,
    column: event.colno,
  });
const reportRejection = (event: PromiseRejectionEvent) =>
  reportFrontendFault("unhandledrejection", {
    errorType: faultType(event.reason),
  });
window.addEventListener("error", reportError);
window.addEventListener("unhandledrejection", reportRejection);
// 开发热更新需移除旧监听，防止同一故障被重复上报；生产只有当前这一组监听。
if (import.meta.hot)
  import.meta.hot.dispose(() => {
    window.removeEventListener("error", reportError);
    window.removeEventListener("unhandledrejection", reportRejection);
  });

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
