/** Sonner 悬浮反馈：顶部居中、脱离文档流，业务布局不随通知出现/关闭而改变。@author ahui */
import { useEffect, useLayoutEffect, useState } from "react";
import { createPortal } from "react-dom";
import { Toaster } from "sonner";
import type { ThemeChoice } from "../lib/theme";

/**
 * 把单一悬浮宿主移入最上层原生模态框；无弹窗时附在应用根节点。
 * 宿主固定覆盖视口但不拦截指针，仅通知卡片可交互；长内容独立滚动，关闭键固定在卡片内。
 * 原生 showModal 的层级高于普通 z-index，故仍需移动同一宿主，不能仅提高CSS层级。
 * @param props 外观跟随应用；system 由本组件解析并解绑监听，避免第三方组件残留媒体订阅。
 * @returns 全局悬浮反馈；沿用Sonner的去重、时限、手动关闭和朗读，不重新创建业务状态。
 */
export default function FeedbackHost({ theme }: { theme: ThemeChoice }) {
  const [container] = useState(() => document.createElement("div"));
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );

  useEffect(() => {
    if (theme !== "system") return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => setSystemDark(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [theme]);

  useLayoutEffect(() => {
    container.className = "feedback-layer";
    const updateTarget = () => {
      const dialogs = document.querySelectorAll("dialog[open]");
      const target =
        dialogs.item(dialogs.length - 1) ??
        document.querySelector(".app-shell") ??
        document.body;
      // 移动同一宿主而不重建Toaster，保留时限和原生焦点；追加到末尾不影响标题的首子节点样式。
      // 宿主由CSS脱离布局，进入dialog是为保证它在浏览器原生顶层可见、可关闭。
      if (container.parentElement !== target) target.append(container);
    };
    const observer = new MutationObserver(updateTarget);
    observer.observe(document.body, {
      subtree: true,
      attributes: true,
      attributeFilter: ["open"],
    });
    updateTarget();
    return () => {
      observer.disconnect();
      container.remove();
    };
  }, [container]);

  return createPortal(
    <Toaster
      // Sonner 2.0.8 的 system 分支不清理媒体监听；始终传显式颜色并由上面的 effect 管理生命周期。
      theme={theme === "system" ? (systemDark ? "dark" : "light") : theme}
      // 只覆盖浮层边界与主题；堆叠/测量/退出继续交给Sonner，避免复制第三方生命周期。
      className="app-toaster"
      position="top-center"
      expand
      offset={16}
      mobileOffset={16}
      closeButton
      richColors
      visibleToasts={3}
      containerAriaLabel="操作提示"
      toastOptions={{
        closeButtonAriaLabel: "关闭提示",
        classNames: { toast: "app-toast" },
      }}
    />,
    container,
  );
}
