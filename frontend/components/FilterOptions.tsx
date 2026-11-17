/** 目标群的可选过滤条件；多群排除按 QQ 号集合处理，页面只传选择、不自行凑名单。@author ahui */
import { ChevronDown, Search, X } from "lucide-react";
import { useRef } from "react";
import type { Group } from "../types";

/** 过滤区所需的当前选择；获取期间所有变更入口由 disabled 锁定。 */
interface Props {
  groups: Group[];
  targetId: string;
  /** 多目标群全部从排除候选移除；旧单群调用可省略。 */
  targetIds?: string[];
  excludeIds: string[];
  query: string;
  open: boolean;
  excludeSent: boolean;
  excludeFriends: boolean;
  disabled: boolean;
  onOpen: () => void;
  onQuery: (value: string) => void;
  onChange: (ids: string[]) => void;
  onSent: (value: boolean) => void;
  onFriends: (value: boolean) => void;
}

/** 显示当前账号加入的其他群；无法获取的排除群由后台返回失败，不静默忽略。 */
export default function FilterOptions(props: Props) {
  const searchInput = useRef<HTMLInputElement>(null);
  const available = props.groups.filter(
    (group) => !(props.targetIds ?? [props.targetId]).includes(group.id),
  );
  const matched = available.filter((group) =>
    `${group.name} ${group.id}`
      .toLocaleLowerCase()
      .includes(props.query.trim().toLocaleLowerCase()),
  );
  const matchedSelectedCount = matched.filter((group) =>
    props.excludeIds.includes(group.id),
  ).length;

  /**
   * 批量操作只影响当前搜索结果；其他搜索条件下选中的群保持原样，目标群始终不参与。
   * @param checked true 选中匹配群，false 只取消这些匹配群的选择。
   */
  function selectMatched(checked: boolean) {
    if (props.disabled || !matched.length) return;
    const next = new Set(props.excludeIds);
    for (const group of matched) {
      if (checked) next.add(group.id);
      else next.delete(group.id);
    }
    props.onChange([...next]);
  }

  return (
    <section className="filter-options" aria-label="名单过滤选项">
      <h3>名单筛选</h3>
      <div className="filter-main-row">
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={props.excludeSent}
            disabled={props.disabled}
            onChange={(event) => props.onSent(event.target.checked)}
          />
          <span>过滤已发送记录</span>
        </label>
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={props.excludeFriends}
            disabled={props.disabled}
            onChange={(event) => props.onFriends(event.target.checked)}
          />
          <span>排除我的 QQ 好友</span>
        </label>
      </div>
      <button
        className="filter-expand"
        disabled={props.disabled}
        onClick={props.onOpen}
        aria-expanded={props.open}
      >
        <ChevronDown size={15} className={props.open ? "rotate" : ""} />
        <span>排除其他群的成员</span>
        {props.excludeIds.length > 0 && (
          <span className="secondary">已选 {props.excludeIds.length} 个</span>
        )}
      </button>
      {props.open && (
        <div className="exclusion-picker">
          <div className="search-field">
            <Search size={15} />
            <input
              ref={searchInput}
              aria-label="搜索排除群"
              placeholder="搜索要排除的群"
              value={props.query}
              disabled={props.disabled}
              onChange={(event) => props.onQuery(event.target.value)}
            />
            {props.query && (
              <button
                className="icon-button"
                aria-label="清除排除群搜索"
                disabled={props.disabled}
                onClick={() => {
                  props.onQuery("");
                  searchInput.current?.focus();
                }}
              >
                <X size={14} aria-hidden="true" />
              </button>
            )}
          </div>
          <div className="exclusion-selection-actions">
            <button
              className="button"
              disabled={props.disabled || !matched.length}
              aria-pressed={
                matched.length > 0 && matchedSelectedCount === matched.length
              }
              onClick={() =>
                selectMatched(matchedSelectedCount !== matched.length)
              }
            >
              {matched.length > 0 && matchedSelectedCount === matched.length
                ? "取消全选（当前结果）"
                : "全选当前结果"}
            </button>
            {props.excludeIds.length > 0 && (
              <button
                className="button"
                disabled={props.disabled}
                onClick={() => props.onChange([])}
              >
                清空全部选择
              </button>
            )}
            <span className="secondary" role="status">
              当前结果 {matched.length} 个 · 已选 {matchedSelectedCount} 个
              {props.query.trim() &&
              props.excludeIds.length > matchedSelectedCount
                ? ` · 搜索范围外已选 ${props.excludeIds.length - matchedSelectedCount} 个`
                : ""}
            </span>
          </div>
          <div className="exclusion-options">
            {matched.length ? (
              matched.map((group) => (
                <label className="exclusion-option" key={group.id}>
                  <input
                    type="checkbox"
                    checked={props.excludeIds.includes(group.id)}
                    disabled={props.disabled}
                    onChange={(event) =>
                      props.onChange(
                        event.target.checked
                          ? [...props.excludeIds, group.id]
                          : props.excludeIds.filter((id) => id !== group.id),
                      )
                    }
                  />
                  <span>
                    {group.name.trim() || "未命名群"}
                    <small>{group.id}</small>
                  </span>
                </label>
              ))
            ) : (
              <p className="secondary">
                {available.length ? "没有匹配的群。" : "当前没有其他可选群。"}
              </p>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
