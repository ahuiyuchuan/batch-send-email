/** 页面、原生标题栏与本机外观偏好的统一入口。只保存外观枚举，不保存业务数据。@author ahui */
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** system 将原生窗口交回操作系统管理，页面通过现有媒体查询跟随系统。 */
export type ThemeChoice = "system" | "light" | "dark";

const THEME_STORAGE_KEY = "qq-group-exporter-theme";
// 快速切换时按请求顺序调用原生 API；失败不能阻塞后续切换，也不能让旧请求覆盖最新外观。
let nativeThemeQueue: Promise<void> = Promise.resolve();
let newestThemeRequest = 0;

/**
 * 读取上次明确选择的外观；首次运行、无效值或浏览器存储不可读时跟随系统。
 * @returns light、dark 或 system；不会返回任意存储字符串或因读取失败中断启动。
 */
export function getSavedTheme(): ThemeChoice {
  try {
    const saved = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // 外观不是业务凭据；无法读取偏好时仍可正常使用系统主题。
  }
  return "system";
}

/**
 * 同步页面、下次启动偏好和 Windows 原生标题栏；普通浏览器只更新页面与偏好。
 *
 * 页面立即切换，原生调用串行且只应用排队中的最新请求。system 必须传 null，
 * 不能把当前系统颜色固化成 light/dark，否则后续操作系统主题变化无法正确跟随。
 * @param theme 用户选择的外观枚举，不接受未经校验的 localStorage 字符串。
 * @returns 同步完成后兑现；原生设置或偏好保存失败时拒绝，交由操作入口显示一次提示。
 */
export async function applyTheme(theme: ThemeChoice): Promise<void> {
  document.documentElement.dataset.theme = theme;
  let storageFailed = false;
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // 即使偏好写入失败，也继续切换原生标题栏，避免当前窗口出现深浅色不一致。
    storageFailed = true;
  }

  if (isTauri()) {
    const request = ++newestThemeRequest;
    const update = nativeThemeQueue
      .catch(() => undefined)
      .then(async () => {
        if (request !== newestThemeRequest) return;
        await getCurrentWindow().setTheme(theme === "system" ? null : theme);
      });
    nativeThemeQueue = update;
    try {
      await update;
    } catch {
      throw new Error("窗口外观未能同步，请重试切换外观。");
    }
  }

  if (storageFailed) {
    throw new Error("外观已切换，但未能保存设置，下次启动可能恢复为跟随系统。");
  }
}
