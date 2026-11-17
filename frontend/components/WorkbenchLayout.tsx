/**
 * 工作台的统一分栏：左侧配置/维护，右侧编辑/结果，两区独立滚动，页脚由业务页面固定在外侧。
 * 只负责布局，不持有名单、草稿、选择或任务状态，切换和重排不会复制业务数据。
 * @author ahui
 */
import type { ReactNode } from "react";

interface Props {
  sidebar: ReactNode;
  /** 可选的固定栏头，与下面的滚动区分开布局，避免吸顶控件覆盖内容或焦点。 */
  sidebarHeader?: ReactNode;
  sidebarLabel: string;
  contentLabel: string;
  children: ReactNode;
  className?: string;
}

/**
 * 使用同一双栏契约承载写信、发送记录与手动排除，滚动区名称同时供键盘和自动化定位。
 * @param props sidebar为配置区，sidebarHeader为可选固定栏头，children为主内容；标签应描述实际操作，不能传入用户敏感数据。
 * @returns 不接管焦点、表单提交或生命周期的语义布局；隐藏工作页时由上层统一控制。
 */
export default function WorkbenchLayout({
  sidebar,
  sidebarHeader,
  sidebarLabel,
  contentLabel,
  children,
  className = "",
}: Props) {
  return (
    <div className={`workbench-layout ${className}`}>
      <div className="workbench-sidebar-stack">
        {sidebarHeader && (
          <div className="workbench-sidebar-header">{sidebarHeader}</div>
        )}
        <aside
          className="workbench-sidebar"
          aria-label={sidebarLabel}
          tabIndex={0}
        >
          {sidebar}
        </aside>
      </div>
      <div
        className="workbench-content"
        role="region"
        aria-label={contentLabel}
        tabIndex={0}
      >
        {children}
      </div>
    </div>
  );
}
