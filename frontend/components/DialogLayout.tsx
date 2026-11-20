/**
 * 模态框的统一内容布局：标题和操作区保持可见，仅中间内容滚动。
 * 原生dialog的ref、打开/关闭、取消门禁继续归业务组件管理；不接管焦点或创建额外状态。
 * @author ahui
 */
import type { ReactNode } from "react";

interface Props {
  heading: ReactNode;
  children: ReactNode;
  footer: ReactNode;
}

/**
 * 直接返回三段内容，保证它们仍是原生dialog的直接子节点。
 * @param props heading为标题/关闭入口，footer为显式dialog-footer操作区，children可为空。
 * @returns 不含额外外壳的布局；悬浮通知宿主附在末尾且不占位，底部操作区仍以显式类名定位。
 */
export default function DialogLayout({ heading, children, footer }: Props) {
  return (
    <>
      {heading}
      <div className="dialog-body">{children}</div>
      {footer}
    </>
  );
}
