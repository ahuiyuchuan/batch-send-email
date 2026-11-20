/** 目标群组合的展示/缓存键；与宿主multi_group::selection_key保持一致，不生成发送名单。@author ahui */
import type { Snapshot } from "../types";

/** 顺序无关且不修改传入数组；空选择没有缓存键，单群沿用原ID。 */
export function groupSelectionKey(ids: readonly string[]): string {
  const unique = [...new Set(ids)].sort();
  return unique.length > 1 ? `multi:${unique.join(",")}` : (unique[0] ?? "");
}

/** 实际群来源ID；组合存储键不能显示为QQ群号或提交为网络目标群。 */
export function snapshotGroupIds(snapshot: Snapshot): string[] {
  return snapshot.sourceGroups?.length
    ? snapshot.sourceGroups.map((source) => source.group.id)
    : [snapshot.group.id];
}

/** 简洁来源说明供邮件确认、替换和返回来源使用，不将内部组合键暴露为群号。 */
export function snapshotGroupLabel(snapshot: Snapshot): string {
  return snapshot.sourceGroups?.length
    ? snapshot.sourceGroups
        .map(
          (source) =>
            `${source.group.name || "未命名群"}（${source.group.id}）`,
        )
        .join("、")
    : `群号 ${snapshot.group.id}`;
}
