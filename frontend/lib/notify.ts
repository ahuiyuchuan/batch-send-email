/** 应用操作反馈统一入口；只接收已脱敏的用户提示，不传入凭据或完整业务对象。@author ahui */
import { toast } from "sonner";

export interface Notification {
  message: string;
  tone: "error" | "success" | "info";
}

/** 仅对仍活跃的同文提示去重；关闭/超时立即释放，不复用仍在退出动画中的第三方条目ID。 */
const activeNotifications = new Map<string, string>();
let notificationSequence = 0;

/**
 * 显示自动收起的浮层；尚未关闭的同类同文提示复用一个条目，避免并行回调堆满屏幕。
 * 用户关闭或提示超时后使用新ID：Sonner的旧条目会延迟卸载，复用旧ID会被其迟到清理误删。
 * 字段校验与待处理状态仍由对应控件展示，调用方不能仅靠临时浮层保存关键业务状态。
 * @param notification 已完成操作的安全摘要；错误显示七秒，其余显示四秒，均可手动关闭。
 */
export function notify({ message, tone }: Notification): void {
  const content = message.trim();
  if (!content) return;
  const key = `${tone}:${content}`;
  const id =
    activeNotifications.get(key) ?? `app-notice:${++notificationSequence}`;
  activeNotifications.set(key, id);
  // 旧条目的迟到回调不能释放后来同文提示的登记；只清理它自己持有的ID。
  const release = () => {
    if (activeNotifications.get(key) === id) activeNotifications.delete(key);
  };
  toast[tone](content, {
    id,
    duration: tone === "error" ? 7000 : 4000,
    onDismiss: release,
    onAutoClose: release,
  });
}
