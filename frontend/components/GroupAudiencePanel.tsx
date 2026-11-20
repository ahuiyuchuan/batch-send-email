/**
 * 写信页中的固定群收件范围。只展示已经由成员工作区确认的名单，搜索和分页
 * 不改变收件范围；调整操作返回成员工作区，避免在两处维护互相矛盾的勾选状态。
 * @author ahui
 */
import { ChevronLeft, ChevronRight, Search, Users, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { displayTime, filterMembers, sortListByLabel } from "../lib/view";
import type { GroupMailAudience } from "../mail-types";
import type { Snapshot } from "../types";
import { snapshotGroupLabel } from "../lib/group-selection";

interface Props {
  audience: GroupMailAudience | null;
  currentSnapshot: Snapshot | null;
  disabled: boolean;
  onChooseMembers: () => void;
}

/**
 * 显示来源、排除条件和本封群收件人。保留旧快照是为了让重新获取不会悄悄换人，
 * 不代表旧结果已验证完整或邮箱一定开通。
 */
export default function GroupAudiencePanel({
  audience,
  currentSnapshot,
  disabled,
  onChooseMembers,
}: Props) {
  const [expanded, setExpanded] = useState(false);
  const [query, setQuery] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const [page, setPage] = useState(1);
  const selectedMembers = useMemo(() => {
    if (!audience) return [];
    const chosen = new Set(audience.qqNumbers);
    // 展示顺序与成员页一致；不能重排已冻结的发送范围或账号分配。
    return sortListByLabel(
      audience.snapshot.members.filter((member) => chosen.has(member.qqNumber)),
      (member) => member.qqNumber,
      (member) => member.qqNumber,
    );
  }, [audience]);
  const matches = useMemo(
    () => filterMembers(selectedMembers, query),
    [selectedMembers, query],
  );
  const pages = Math.max(1, Math.ceil(matches.length / 20));
  const currentPage = Math.min(page, pages);
  const visible = matches.slice((currentPage - 1) * 20, currentPage * 20);

  // 同版本刷新可能减少人数，钳制页码后不能在人数增长时自动跳回原末页。
  useEffect(() => {
    setPage((current) => Math.min(current, pages));
  }, [pages]);

  useEffect(() => {
    setQuery("");
    setPage(1);
    // 同一快照的规则重算只更新内容，保留正在使用的搜索；切换真实版本才重置查询。
  }, [audience?.snapshot.id]);

  if (!audience) {
    return (
      <section
        className="mail-audience mail-audience-empty"
        aria-label="选择群收件人"
      >
        <div className="mail-audience-symbol">
          <Users size={22} aria-hidden="true" />
        </div>
        <div>
          <h3>先选择群成员</h3>
        </div>
        <button
          className="button primary"
          disabled={disabled}
          onClick={onChooseMembers}
        >
          选择群成员
        </button>
      </section>
    );
  }

  const source = audience.snapshot;
  // 查看其他群不会改变本封邮件；只有同账号同群出现了新版本才提示名单更新。
  const changed =
    currentSnapshot?.group.id === source.group.id &&
    currentSnapshot.account.qqNumber === source.account.qqNumber &&
    currentSnapshot.account.source === source.account.source &&
    currentSnapshot.id !== source.id;
  const uncertainExclusion = source.exclusions.some(
    (exclusion) => exclusion.status !== "matched",
  );
  const uncertainFriends =
    !!source.excludeFriends && source.friendFilterStatus !== "completed";
  const uncertain =
    source.status !== "matched" || uncertainExclusion || uncertainFriends;

  return (
    <section className="mail-audience" aria-labelledby="mail-audience-heading">
      <div className="mail-audience-heading">
        <div>
          <h3 id="mail-audience-heading">
            <Users size={17} aria-hidden="true" />
            {source.group.name || "未命名群"}
          </h3>
          <p title={snapshotGroupLabel(source)}>
            {source.sourceGroups?.length
              ? `${source.sourceGroups.length}个来源群`
              : snapshotGroupLabel(source)}{" "}
            · {displayTime(source.finishedAt)}
          </p>
          {/* 用一行明确本封范围，避免两张人数卡片和页尾重复解释同一个选择。 */}
          <p className="mail-audience-counts">
            本封已选 <strong>{selectedMembers.length}</strong> / 筛选后{" "}
            {source.members.length} 人
          </p>
        </div>
        <button
          className="button"
          disabled={disabled}
          onClick={onChooseMembers}
        >
          调整群成员
        </button>
      </div>
      {/* 筛选过程供需要核对时展开；影响本封范围的待核验和版本更新仍持续显示，不能用临时提示替代。 */}
      {uncertain && (
        <p className="mail-audience-warning">
          来源名单{source.status === "partial" ? "不完整" : "仍待核验"}
          {uncertainExclusion ? "，排除群也有待核验项" : ""}
          {uncertainFriends ? "，好友排除未完成核验" : ""}
          ，本次仅发送给已选成员。
        </p>
      )}
      {changed && (
        <p className="mail-audience-warning" role="status">
          该群已有更新的名单，本封仍使用已确认版本。可点击“调整群成员”重新选择。
        </p>
      )}
      <div className="mail-audience-footer">
        <details className="mail-audience-details">
          <summary>筛选详情</summary>
          {!!source.sourceGroups?.length && (
            <p className="mail-audience-filter-note">
              来源群：{snapshotGroupLabel(source)}
            </p>
          )}
          <p className="mail-audience-filter-note">
            群成员 {source.sourceCount} 人 · 排除重合 {source.excludedCount} 人
            · 过滤已发送 {source.sentFilteredCount} 人
            {(source.manualFilteredCount ?? 0) > 0
              ? ` · 手动排除 ${source.manualFilteredCount} 人`
              : ""}
          </p>
          {(source.excludeFriends || (source.friendFilteredCount ?? 0) > 0) && (
            <p className="mail-audience-filter-note">
              {uncertainFriends
                ? "好友排除未完成核验"
                : `已排除获取账号的 QQ 好友 ${source.friendFilteredCount ?? 0} 人`}
            </p>
          )}
          {source.exclusions.length > 0 && (
            <p className="mail-audience-filter-note">
              排除群：
              {source.exclusions
                .map((exclusion) => exclusion.group.name || exclusion.group.id)
                .join("、")}
            </p>
          )}
        </details>
        <button
          className="mail-text-action"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded
            ? "收起群收件人"
            : `查看群收件人（${selectedMembers.length}）`}
        </button>
      </div>
      {expanded && (
        <section className="mail-audience-roster" aria-label="本封群收件人明细">
          <div className="mail-audience-search">
            <Search size={15} aria-hidden="true" />
            <input
              ref={searchInput}
              aria-label="搜索本封群收件人"
              placeholder="搜索 QQ 号、昵称或名片"
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setPage(1);
              }}
            />
            {query && (
              <button
                className="icon-button"
                aria-label="清除群收件人搜索"
                onClick={() => {
                  setQuery("");
                  setPage(1);
                  searchInput.current?.focus();
                }}
              >
                <X size={14} aria-hidden="true" />
              </button>
            )}
          </div>
          <div className="mail-audience-table-wrap">
            <table>
              <thead>
                <tr>
                  <th>群成员</th>
                  <th>QQ 号</th>
                  <th>候选收件邮箱</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((member) => (
                  <tr key={member.qqNumber}>
                    <td>{member.card || member.nickname || "未设置昵称"}</td>
                    <td>{member.qqNumber}</td>
                    <td>{member.qqNumber}@qq.com</td>
                  </tr>
                ))}
                {!visible.length && (
                  <tr>
                    <td colSpan={3}>
                      {query.trim()
                        ? "没有匹配的群收件人。"
                        : "本封未选择群收件人。"}
                      {query && (
                        <button
                          className="button"
                          onClick={() => {
                            setQuery("");
                            setPage(1);
                            searchInput.current?.focus();
                          }}
                        >
                          清除搜索
                        </button>
                      )}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          <div className="mail-audience-pagination">
            <small>
              共 {selectedMembers.length} 人
              {query ? ` · 匹配 ${matches.length} 人` : ""}
            </small>
            <div>
              <button
                className="icon-button"
                disabled={currentPage <= 1}
                aria-label="上一页群收件人"
                onClick={() => setPage(currentPage - 1)}
              >
                <ChevronLeft size={16} />
              </button>
              <span>
                {currentPage} / {pages}
              </span>
              <button
                className="icon-button"
                disabled={currentPage >= pages}
                aria-label="下一页群收件人"
                onClick={() => setPage(currentPage + 1)}
              >
                <ChevronRight size={16} />
              </button>
            </div>
          </div>
        </section>
      )}
    </section>
  );
}
