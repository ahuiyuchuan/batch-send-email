/** 只负责展示的数据整理，不改变后台结果或导出范围。@author ahui */
import type { Member } from "../types";

import { pinyin } from "pinyin-pro";

/** 只识别汉字，未收录的生僻字保持原字符，落入其他字符的码点顺序。 */
const HAN_CHARACTER = /^[\p{Unified_Ideograph}〇]$/u;

/**
 * 产生中英文共用的排序键：汉字逐字转首字母，ASCII 英文折叠为小写，其余原样保留。
 * 逐字读取固定词库的默认音，不做姓名/词义判断；如 A 与阿同为 a，B 与波同为 b。
 * 不缓存用户名称，调用方排序时每行只计算一次，避免比较过程中反复转换或无限缓存。
 */
function listLabelKey(value: string): string {
  return Array.from(value.trim(), (character) => {
    if (HAN_CHARACTER.test(character)) {
      const initial = pinyin(character, { pattern: "first", toneType: "none" });
      if (/^[a-z]$/.test(initial)) return initial;
    }
    return character.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
  }).join("");
}

/**
 * 比较列表字段：数字在前，英文和汉字拼音首字母共用 A–Z，其余按 Unicode 码点，空白末尾。
 * 中文逐字转默认拼音首字母，与英文同级比较；相同字母继续比较后续字符/数字，键相同返回 0。
 * 连续数字不转 Number，避免长 QQ 号精度丢失；不改变原文、快照、发送分配或导出范围。
 * @param left 左侧字段，首尾空白只在比较时忽略。
 * @param right 右侧字段，规则与 left 相同。
 * @returns 负数表示左侧在前，正数表示右侧在前；同首字母键/大小写/前导零等价返回 0。
 */
export function compareListLabels(left: string, right: string): number {
  return compareLabelKeys(listLabelKey(left), listLabelKey(right));
}

/** 比较已预取的键；全量排序使用此处，数字自然序与辅助平面按原语义处理。 */
function compareLabelKeys(a: string, b: string): number {
  if (!a || !b) return a ? -1 : b ? 1 : 0;
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    const ac = a.codePointAt(i)!;
    const bc = b.codePointAt(j)!;
    const aDigit = ac >= 48 && ac <= 57;
    const bDigit = bc >= 48 && bc <= 57;
    const aLetter = (ac >= 65 && ac <= 90) || (ac >= 97 && ac <= 122);
    const bLetter = (bc >= 65 && bc <= 90) || (bc >= 97 && bc <= 122);
    const rank =
      (aDigit ? 0 : aLetter ? 1 : 2) - (bDigit ? 0 : bLetter ? 1 : 2);
    if (rank) return rank;
    if (aDigit && bDigit) {
      const aStart = i;
      const bStart = j;
      while (i < a.length && a.charCodeAt(i) >= 48 && a.charCodeAt(i) <= 57)
        i++;
      while (j < b.length && b.charCodeAt(j) >= 48 && b.charCodeAt(j) <= 57)
        j++;
      const aNumber = a.slice(aStart, i).replace(/^0+/, "") || "0";
      const bNumber = b.slice(bStart, j).replace(/^0+/, "") || "0";
      if (aNumber.length !== bNumber.length)
        return aNumber.length - bNumber.length;
      if (aNumber !== bNumber) return aNumber < bNumber ? -1 : 1;
      continue;
    }
    const aCode = aLetter && ac <= 90 ? ac + 32 : ac;
    const bCode = bLetter && bc <= 90 ? bc + 32 : bc;
    if (aCode !== bCode) return aCode - bCode;
    // Unicode 辅助平面字符占两个 UTF-16 单元，必须整体前进，不能拆成代理项排序。
    i += ac > 0xffff ? 2 : 1;
    j += bc > 0xffff ? 2 : 1;
  }
  return (i < a.length ? 1 : 0) - (j < b.length ? 1 : 0);
}

/**
 * 创建展示专用的排序副本，再由调用方搜索和分页；原始快照、发送计划、导出范围均不变。
 * 同名项目用持久 ID 排序，避免后台返回顺序变化导致列表跳动；每次仅预取一次字段。
 * @param items 只读源集合，可以为空；对象本身不复制、不修改。
 * @param label 提取列表主要字段，例如群名、模板名或收件账号。
 * @param identity 提取稳定业务 ID；不能使用显示行号，导入预览可用本次固定的原包索引。
 * @returns 按统一规则排列的新数组；调用方应按源集合引用缓存，避免输入时反复排序。
 */
export function sortListByLabel<T>(
  items: readonly T[],
  label: (item: T) => string,
  identity: (item: T) => string | number,
): T[] {
  return items
    .map((item) => {
      const id = String(identity(item));
      return {
        item,
        label: listLabelKey(label(item)),
        id,
        idKey: listLabelKey(id),
      };
    })
    .sort(
      (a, b) =>
        compareLabelKeys(a.label, b.label) ||
        compareLabelKeys(a.idKey, b.idKey) ||
        (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
    )
    .map(({ item }) => item);
}

/** 默认隐藏号码中间段；完整号码仍由当前用户通过明确的显示操作查看。 */
export function maskedNumber(value: string, revealed: boolean): string {
  if (revealed || !value) return value;
  if (value.length <= 4) return "*".repeat(value.length);
  return `${value.slice(0, 2)}${"*".repeat(Math.min(6, value.length - 4))}${value.slice(-2)}`;
}

/** 本地匹配预览；无搜索时复用输入数组，始终不修改原始快照或导出条数。 */
export function filterMembers(members: Member[], query: string): Member[] {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) return members;
  return members.filter((member) =>
    [member.qqNumber, member.nickname, member.card].some((value) =>
      value.toLocaleLowerCase().includes(normalized),
    ),
  );
}

/** 将带时区的后台时间转为本机易读格式；异常值按“时间未知”处理。 */
export function displayTime(value: string): string {
  const time = new Date(value);
  return Number.isNaN(time.getTime())
    ? "时间未知"
    : new Intl.DateTimeFormat("zh-CN", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
      }).format(time);
}
