/**
 * 全局工具栏：业务工作区、关于、当前QQ身份和应用操作只出现一次。
 * 业务状态由App传入，组件只展示并发出用户动作，不读取数据库或启动原生任务。
 * @author ahui
 */
import {
  FileText,
  History,
  Info,
  LogOut,
  Mail,
  Monitor,
  Moon,
  QrCode,
  Sun,
  UserMinus,
  Users,
} from "lucide-react";
import appIcon from "../assets/app-icon.png";
import { maskedNumber } from "../lib/view";
import type { ThemeChoice } from "../lib/theme";
import type { Account } from "../types";

/** 主导航使用固定工作区标识；切换页面只改变可见性，不卸载业务状态。 */
export type WorkspacePage = "members" | "mail" | "history" | "manual" | "about";
interface Props {
  activePage: WorkspacePage;
  account: Account | null;
  expired: boolean;
  revealed: boolean;
  version: string;
  theme: ThemeChoice;
  navigationBusy: boolean;
  locked: boolean;
  mailRunning: boolean;
  audienceCount: number | null;
  historyCount: number;
  logAvailable: boolean;
  /** 宿主已成功发现正式新版时标记关于入口；不使用检查失败推测更新状态。 */
  updateAvailable: boolean;
  onNavigate: (page: WorkspacePage) => void;
  onLogin: () => void;
  onLogout: () => void;
  onTheme: () => void;
  onLogs: () => void;
}

/**
 * 单行工具栏集中呈现选中态、内容计数和独立应用动作，保持导航位置稳定。
 * @param props 所有动作沿用宿主门禁；导航不会卸载已有草稿、名单或后台任务。
 * @returns 全局header；按钮使用稳定中文名称，图标动作同时提供title与可访问名称。
 */
export default function AppToolbar(props: Props) {
  const pages = [
    { id: "members", label: "成员提取", icon: Users },
    { id: "mail", label: "写邮件", icon: Mail },
    { id: "history", label: "已发送记录", icon: History },
    { id: "manual", label: "手动排除", icon: UserMinus },
    { id: "about", label: "关于", icon: Info },
  ] as const;
  const appearance = `外观：${props.theme === "system" ? "跟随系统" : props.theme === "light" ? "浅色" : "深色"}，点击切换`;
  const ThemeIcon =
    props.theme === "system" ? Monitor : props.theme === "light" ? Sun : Moon;
  return (
    <header className="app-header">
      <div className="brand" title={`batch-send-email ${props.version}`}>
        <span className="app-mark">
          <img src={appIcon} alt="" draggable={false} />
        </span>
        <h1>batch-send-email</h1>
        <span className="sr-only" aria-label={`当前版本 ${props.version}`}>
          v{props.version}
        </span>
      </div>
      <nav className="app-nav" aria-label="主要功能">
        {pages.map((page) => (
          <button
            key={page.id}
            aria-label={page.label}
            aria-current={props.activePage === page.id ? "page" : undefined}
            disabled={props.navigationBusy}
            onClick={() => props.onNavigate(page.id)}
          >
            <page.icon size={17} aria-hidden="true" />
            {page.label}
            {page.id === "mail" && props.audienceCount !== null && (
              <span className="nav-count">{props.audienceCount} 人</span>
            )}
            {page.id === "history" && (
              <span className="nav-count">{props.historyCount}</span>
            )}
            {page.id === "mail" && props.mailRunning && (
              <span className="nav-task-dot" aria-label="发送任务进行中" />
            )}
            {page.id === "about" && props.updateAvailable && (
              <span className="nav-update-dot" aria-label="有新版本" />
            )}
          </button>
        ))}
      </nav>
      <div className="header-actions">
        {props.account ? (
          <div
            className="account"
            aria-label="当前已登录账号"
            title="当前已登录，可重新扫码切换账号"
          >
            <span className="online-dot" />
            <div>
              <strong>{props.account.nickname || "QQ"}</strong>
              <span>
                QQ {maskedNumber(props.account.qqNumber, props.revealed)}
              </span>
            </div>
          </div>
        ) : (
          <span className="connection-status" role="status">
            <span className="offline-dot" />
            {props.expired ? "登录已失效" : "尚未登录"}
          </span>
        )}
        <button
          className="text-button"
          disabled={props.locked}
          onClick={props.onLogin}
        >
          <QrCode size={16} aria-hidden="true" />
          {props.account ? "重新扫码" : "扫码登录"}
        </button>
        <span className="toolbar-divider" aria-hidden="true" />
        <button
          className="icon-button"
          aria-label={appearance}
          title={appearance}
          onClick={props.onTheme}
        >
          <ThemeIcon size={18} aria-hidden="true" />
        </button>
        <button
          className={`icon-button log-viewer-button${props.logAvailable ? "" : " log-unavailable"}`}
          aria-label="查看日志"
          title={
            props.logAvailable ? "查看日志" : "日志暂不可写入，查看已有日志"
          }
          onClick={props.onLogs}
        >
          <FileText size={18} aria-hidden="true" />
        </button>
        {props.account && (
          <button
            className="icon-button"
            disabled={props.locked}
            aria-label="退出当前账号"
            title="退出当前账号"
            onClick={props.onLogout}
          >
            <LogOut size={18} aria-hidden="true" />
          </button>
        )}
      </div>
    </header>
  );
}
