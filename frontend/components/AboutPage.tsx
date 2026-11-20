/**
 * 应用信息、正式版本检查和项目入口；沿用本项目的页面结构和视觉系统。
 * 页面不重建工作区、不安装更新，发布说明按纯文本展示，下载只打开经过宿主校验的发布页。
 * @author ahui
 */
import { useRef, useState } from "react";
import {
  ArrowUpRight,
  ExternalLink,
  Github,
  Info,
  LoaderCircle,
  MessageSquare,
  RefreshCw,
  ShieldCheck,
  Sparkles,
} from "lucide-react";
import appIcon from "../assets/app-icon.png";
import { call, errorText } from "../lib/bridge";
import { notify } from "../lib/notify";
import { displayTime } from "../lib/view";
import { PROJECT_URL, type UpdateResult } from "../lib/updates";
import WorkspaceHeader from "./WorkspaceHeader";

interface Props {
  version: string;
  result: UpdateResult | null;
  checking: boolean;
  error: string;
  onCheck: () => Promise<void>;
}

/** 只读页面与任务互不锁定；打开链接有独立防重门闩，失败使用全局顶部悬浮反馈。 */
export default function AboutPage(props: Props) {
  const opening = useRef(false);
  const [openingUrl, setOpeningUrl] = useState("");
  const open = async (url: string) => {
    if (opening.current) return;
    opening.current = true;
    setOpeningUrl(url);
    try {
      await call<void>("open_external_url", { url });
    } catch (error) {
      notify({ message: errorText(error), tone: "error" });
    } finally {
      opening.current = false;
      setOpeningUrl("");
    }
  };
  const link = (url: string) => ({
    disabled: !!openingUrl,
    onClick: () => void open(url),
  });
  const result = props.result;
  return (
    <>
      <div
        className="about-scroll"
        role="region"
        aria-label="关于内容"
        tabIndex={0}
      >
        <div className="about-content">
          <WorkspaceHeader
            headingId="about-heading"
            title="关于"
            actions={<Info size={25} aria-hidden="true" />}
          />
          <section className="about-card about-identity" aria-label="软件信息">
            <img src={appIcon} alt="" draggable={false} />
            <div>
              <h3>batch-send-email</h3>
              <p>QQ群成员整理与多账号邮件发送</p>
              <div className="about-badges">
                <span>v{props.version}</span>
                <span>免安装版 · Windows x64</span>
                <span>GPL-3.0-only</span>
              </div>
            </div>
          </section>
          <section
            className="about-card"
            aria-label="版本更新"
            aria-busy={props.checking}
          >
            <h3>版本更新</h3>
            {props.checking ? (
              <p role="status">正在检查更新…</p>
            ) : props.error ? (
              <p className="about-error" role="alert">
                {props.error}
              </p>
            ) : result?.status === "available" ? (
              <p className="about-available" role="status">
                发现新版本 v{result.latestVersion}
              </p>
            ) : result?.status === "current" ? (
              <p role="status">当前已是最新正式版本。</p>
            ) : result?.status === "no-release" ? (
              <p role="status">项目尚无正式发布版本。</p>
            ) : (
              <p>点击下方“检查更新”，查看最新版本。</p>
            )}
            {result?.status === "available" && result.notes && (
              <pre className="about-release-notes" aria-label="新版本说明">
                {result.notes}
              </pre>
            )}
            {result && (
              <p className="about-checked">
                上次成功检查：{displayTime(result.checkedAt)}
              </p>
            )}
          </section>
          <section className="about-card" aria-label="项目与反馈">
            <h3>项目与反馈</h3>
            <div className="about-links">
              <button className="button" {...link(PROJECT_URL)}>
                <Github size={15} aria-hidden="true" />
                GitHub 项目
              </button>
              <button className="button" {...link(`${PROJECT_URL}/releases`)}>
                <ExternalLink size={15} aria-hidden="true" />
                发布记录
              </button>
              <button className="button" {...link(`${PROJECT_URL}/issues`)}>
                <MessageSquare size={15} aria-hidden="true" />
                问题反馈
              </button>
            </div>
            <p className="about-secondary">
              作者 ahui · 喜欢这个工具，可以在 GitHub 上点亮 Star。
            </p>
          </section>
          <section className="about-card" aria-label="本版功能">
            <h3>
              <Sparkles size={16} aria-hidden="true" />
              本版功能
            </h3>
            <ul>
              <li>
                多选群聊，逐群核验，按 QQ 号合并去重，支持 Excel、CSV 和 TXT
                导出。
              </li>
              <li>
                统一筛选群重合、QQ
                好友、已发送记录与手动排除，支持搜索、全选和成员恢复。
              </li>
              <li>
                多账号随机均分收件人，共享发送记录与排除名单，独立冷却并显示预计结束时间。
              </li>
              <li>正文支持格式、图片和附件，模板支持本机保存、导入和导出。</li>
              <li>
                SQLite
                保存群聊与成员结果，发送记录标明发件账号，待核实投递需明确处理。
              </li>
              <li>
                发送与筛选选项自动记忆，支持深浅外观、顶部悬浮反馈和固定底部操作。
              </li>
            </ul>
          </section>
          <p className="about-privacy">
            <ShieldCheck size={16} aria-hidden="true" />
            <span>
              业务数据保存在本机，SMTP 授权码由 Windows 加密保护。检查更新仅请求
              GitHub 发布信息，不发送账号、成员、邮件或凭据。
            </span>
          </p>
        </div>
      </div>
      <footer className="about-footer">
        <span className="secondary">免安装版 · Windows x64</span>
        <div>
          {!props.error && result?.status === "available" && (
            <button className="button" {...link(result.releaseUrl)}>
              <ArrowUpRight size={16} aria-hidden="true" />
              前往下载 v{result.latestVersion}
            </button>
          )}
          <button
            className="button primary"
            disabled={props.checking}
            aria-busy={props.checking}
            onClick={() => void props.onCheck()}
          >
            {props.checking ? (
              <LoaderCircle size={16} className="spin" aria-hidden="true" />
            ) : (
              <RefreshCw size={16} aria-hidden="true" />
            )}
            {props.checking ? "正在检查…" : "检查更新"}
          </button>
        </div>
      </footer>
    </>
  );
}
