/** 群成员到邮件的交接入口；搜索选择与整份名单分开标注，避免误把搜索当发送范围。@author ahui */
import { Mail, RefreshCw } from "lucide-react";

interface Props {
  total: number;
  selected: number;
  matched: number;
  /** 当前搜索匹配的可选成员是否已全部选中；不包含待核实或状态未知的成员。 */
  allMatchedSelected: boolean;
  query: string;
  unresolved: number;
  sent: number;
  loading: boolean;
  /** 仅实际状态请求占用刷新按钮；状态缺失或读取错误仍应允许用户重试。 */
  refreshing: boolean;
  error: string | null;
  disabled: boolean;
  onAll: () => void;
  onSelected: () => void;
  onSelectMatched: () => void;
  onClear: () => void;
  onRefresh: () => void;
  onHistory: () => void;
  onExclude: () => void;
}

/** 状态未读完时禁止交接，待核实成员必须先处理；已发送成员由发送页默认再次跳过。 */
export default function MemberMailActions(props: Props) {
  const locked = props.disabled || props.loading || !!props.error;
  return (
    <section className="member-mail-actions" aria-label="群成员收件范围">
      <div className="member-mail-heading">
        <div>
          <span aria-live="polite">已选 {props.selected} 人</span>
        </div>
        <div className="member-mail-selection">
          <button
            className="text-button"
            disabled={locked || !props.matched}
            aria-pressed={props.allMatchedSelected}
            title="全选跨页生效；取消全选仅影响当前搜索范围。"
            onClick={props.onSelectMatched}
          >
            {props.allMatchedSelected ? "取消全选" : "全选"}
            {props.query.trim() ? "搜索结果" : "成员"} {props.matched} 人
          </button>
          {props.selected > 0 && (
            <button
              className="text-button"
              disabled={props.disabled || !props.selected}
              onClick={props.onClear}
            >
              清空成员选择
            </button>
          )}
          <button
            className="text-button"
            disabled={props.disabled || props.refreshing}
            onClick={props.onRefresh}
          >
            <RefreshCw size={13} />
            刷新发送状态
          </button>
        </div>
        <div className="member-mail-buttons">
          {props.selected > 0 && (
            <button
              className="button"
              disabled={props.disabled || !props.selected}
              onClick={props.onExclude}
            >
              排除所选 {props.selected} 人
            </button>
          )}
          {props.selected > 0 && (
            <button
              className="button"
              disabled={locked || !props.selected}
              onClick={props.onSelected}
            >
              <Mail size={15} />
              给已选 {props.selected} 人写信
            </button>
          )}
          <button
            className="button primary"
            disabled={locked || !props.total || props.unresolved > 0}
            onClick={props.onAll}
          >
            <Mail size={15} />
            给全部 {props.total} 人写信
          </button>
        </div>
      </div>
      {props.loading ? (
        <p className="secondary" role="status">
          正在核对本机发送状态…
        </p>
      ) : props.error ? (
        <p className="member-mail-warning" role="alert">
          {props.error} 请刷新后再选择收件人。
        </p>
      ) : props.sent > 0 ? (
        <p className="secondary">
          已有发送记录 {props.sent} 人，发送时默认跳过。
        </p>
      ) : null}
      {props.unresolved > 0 && (
        <p className="member-mail-warning">
          {props.unresolved}{" "}
          人存在待核实投递，暂不可勾选或给全部成员写信。可勾选其他成员，或
          <button className="text-button" onClick={props.onHistory}>
            核实投递记录
          </button>
          。
        </p>
      )}
    </section>
  );
}
