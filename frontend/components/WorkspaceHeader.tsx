/** 页面标题与操作区；成员、写信、记录和排除共用同一层级，避免各页叠加说明条。@author ahui */
import type { ReactNode } from "react";

interface Props {
  headingId: string;
  title: string;
  actions?: ReactNode;
  meta?: ReactNode;
  className?: string;
}

/**
 * 只负责语义与排版，不拥有业务状态；标题ID继续作为页面的可访问名称。
 * @param props meta仅传必要的实时数据，不能放操作教程；actions复用既有事件和禁用门禁。
 * @returns 一个标题行，所有动作保持正常DOM顺序以支持键盘、读屏和自动化定位。
 */
export default function WorkspaceHeader({
  headingId,
  title,
  actions,
  meta,
  className = "",
}: Props) {
  return (
    <header className={`workspace-header ${className}`}>
      <div>
        <h2 id={headingId}>{title}</h2>
        {meta && <div className="workspace-meta">{meta}</div>}
      </div>
      {actions}
    </header>
  );
}
