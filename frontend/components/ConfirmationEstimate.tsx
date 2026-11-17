/** 确认前的动态结束时间；仅读取冻结计划，不启动发送、不重新分配收件人。@author ahui */
import { useEffect, useState } from "react";
import { call } from "../lib/bridge";

interface Props {
  planId: string;
  intervalSeconds: number;
  accountSwitchIntervalSeconds: number;
}

/**
 * 仅在确认弹窗打开时挂载，每秒串行估算一次；宿主与实发共用调度规则和冷却副本。
 * 关闭或确认后清理计时和监听，迟到响应丢弃；失败显示可恢复状态，不连续弹出通知。
 */
export default function ConfirmationEstimate(props: Props) {
  const { planId, intervalSeconds, accountSwitchIntervalSeconds } = props;
  const [deadline, setDeadline] = useState<number | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let disposed = false;
    let pending = false;
    let timer: ReturnType<typeof setTimeout>;
    async function update() {
      if (disposed || pending) return;
      clearTimeout(timer);
      pending = true;
      try {
        const value = await call<number | null>(
          "estimate_prepared_mail_completion",
          {
            input: { planId, intervalSeconds, accountSwitchIntervalSeconds },
          },
        );
        if (disposed) return;
        if (
          value === null ||
          !Number.isSafeInteger(value) ||
          value <= 0 ||
          !Number.isFinite(new Date(value).getTime())
        )
          throw new Error("Invalid completion estimate");
        setDeadline(value);
        setFailed(false);
      } catch {
        if (!disposed) {
          setDeadline(null);
          setFailed(true);
        }
      } finally {
        pending = false;
        if (!disposed) timer = setTimeout(update, 1000);
      }
    }
    void update();
    const refresh = () => {
      if (!document.hidden) void update();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      disposed = true;
      clearTimeout(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [planId, intervalSeconds, accountSwitchIntervalSeconds]);
  const date = deadline === null ? null : new Date(deadline);
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    <span
      className="mail-confirm-estimate"
      aria-label="确认发送预计结束时间"
      aria-live="off"
      title="本机时间，按每封约10秒及账号间隔估算；发送后随实际耗时更新。"
    >
      {date ? (
        <time dateTime={date.toISOString()}>
          {date.getFullYear()}年{pad(date.getMonth() + 1)}月
          {pad(date.getDate())}日{pad(date.getHours())}:{pad(date.getMinutes())}
          :{pad(date.getSeconds())}
        </time>
      ) : failed ? (
        "暂无法估算，正在重试"
      ) : (
        "正在估算…"
      )}
    </span>
  );
}
