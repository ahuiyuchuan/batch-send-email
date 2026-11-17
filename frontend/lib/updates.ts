/**
 * 关于页的只读更新状态：启动/后台检查由宿主管理，页面只订阅及发起明确的手动重试。
 * 不接触邮件、登录或本地业务数据；所有外部链接仍由宿主白名单校验。
 * @author ahui
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { call, errorText } from "./bridge";

export const PROJECT_URL = "https://github.com/ahuiyuchuan/batch-send-email";

/** 成功检查结果；无正式发布与已是最新版分别呈现，checkedAt为宿主UTC RFC3339时间。 */
export interface UpdateResult {
  status: "available" | "current" | "no-release";
  currentVersion: string;
  latestVersion: string | null;
  releaseUrl: string;
  notes: string;
  checkedAt: string;
}

/**
 * 常驻App层保留检查结果，切页不重复联网、不清空错误或正在进行的检查。
 * 后台事件优先于迟到的初始读取/手动响应，卸载后不再回填状态；同步ref阻止重复提交。
 * @param desktop 是否存在桌面桥接；浏览器直接打开不读取缓存、不发起更新请求。
 * @returns 已确认结果、手动检查状态及重试入口；失败保留旧成功结果但明确显示错误。
 */
export function useUpdates(desktop: boolean) {
  const [result, setResult] = useState<UpdateResult | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const mounted = useRef(false);
  const pending = useRef(false);
  const eventRevision = useRef(0);

  const apply = useCallback((value: UpdateResult) => {
    if (!mounted.current) return;
    setResult((previous) =>
      previous && Date.parse(previous.checkedAt) > Date.parse(value.checkedAt)
        ? previous
        : value,
    );
  }, []);

  useEffect(() => {
    if (!desktop) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    mounted.current = true;
    void listen<UpdateResult>("update-status-changed", ({ payload }) => {
      if (disposed) return;
      ++eventRevision.current;
      apply(payload);
      setError("");
    })
      .then(async (cleanup) => {
        if (disposed) {
          cleanup();
          return;
        }
        unlisten = cleanup;
        const revision = eventRevision.current;
        const saved = await call<UpdateResult | null>("get_update_status");
        if (!disposed && revision === eventRevision.current && saved)
          apply(saved);
      })
      .catch(() => {
        // 初始缓存读取/事件通道失败不伪造“最新”，用户仍可通过手动检查获得明确结果或错误。
      });
    return () => {
      disposed = true;
      mounted.current = false;
      unlisten?.();
    };
  }, [desktop, apply]);

  const check = async () => {
    if (!desktop || pending.current || !mounted.current) return;
    pending.current = true;
    setChecking(true);
    setError("");
    const revision = eventRevision.current;
    try {
      const next = await call<UpdateResult>("check_updates");
      if (mounted.current && revision === eventRevision.current) apply(next);
    } catch (cause) {
      if (mounted.current && revision === eventRevision.current)
        setError(errorText(cause));
    } finally {
      pending.current = false;
      if (mounted.current) setChecking(false);
    }
  };
  return { result, checking, error, check };
}
