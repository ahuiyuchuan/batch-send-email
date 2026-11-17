/**
 * 统一列表排序的业务回归：直接执行展示层公共比较器，不依赖浏览器语言或数据库。
 * 使用 Node 内置类型擦除（测试脚本显式启用）；只使用合成名称/账号，不包含真实用户数据。
 * @author ahui
 */
import assert from "node:assert/strict";
import test from "node:test";
import { compareListLabels, sortListByLabel } from "../lib/view.ts";

/** 以固定业务 ID 排序合成列表，便于断言主字段和同名顺序。 */
function sortedLabels(labels) {
  return sortListByLabel(
    labels.map((label, id) => ({ label, id })),
    (item) => item.label,
    (item) => item.id,
  ).map((item) => item.label);
}

test("数字优先，中英文共用字母序，其他码点最后，数字采用自然顺序", () => {
  assert.deepEqual(
    sortedLabels([
      "群",
      "b群",
      "10群",
      "中群",
      "A10群",
      "2群",
      "!群",
      "a2群",
      "1群",
    ]),
    ["1群", "2群", "10群", "a2群", "A10群", "b群", "群", "中群", "!群"],
  );
});

test("英文忽略大小写，连续数字可包含前导零", () => {
  assert.equal(compareListLabels("a02", "A2"), 0);
  assert.ok(compareListLabels("a0002b", "A10a") < 0);
  assert.ok(compareListLabels("0", "00a") < 0);
  assert.deepEqual(sortedLabels(["b", "A", "a", "B"]), ["A", "a", "b", "B"]);
});

test("长号码不经过浮点数转换", () => {
  assert.deepEqual(
    sortedLabels([
      "9007199254740993",
      "10000000000000000",
      "9007199254740992",
      "12345",
    ]),
    ["12345", "9007199254740992", "9007199254740993", "10000000000000000"],
  );
});

test("中文按拼音排序，其他字符仍按码点排列，支持辅助平面", () => {
  assert.deepEqual(
    sortedLabels(["😀", "１", "\uE000", "𠀀", "阿", "中", "é", "!"]),
    ["阿", "中", "!", "é", "\uE000", "１", "😀", "𠀀"],
  );
  assert.ok(compareListLabels("😀阿", "😀中") < 0);
});

test("首个汉字按拼音 A–Z，同首字母继续比较并保留数字自然顺序", () => {
  assert.deepEqual(
    sortedLabels(["张群", "中群", "李群", "陈群", "波群", "八群", "阿群"]),
    ["阿群", "波群", "八群", "陈群", "李群", "张群", "中群"],
  );
  assert.deepEqual(
    sortedLabels(["项目10群", "项目2群", "阿中", "阿波", "阿八"]),
    ["阿波", "阿八", "阿中", "项目2群", "项目10群"],
  );
  assert.equal(compareListLabels("張群", "张群"), 0);
  assert.equal(compareListLabels("阿群", "啊群"), 0);
});

/** 中英文首字母等价时使用持久 ID，不能再按原字符类别把全部英文放在中文之前。 */
test("A与阿、B与波同级混排，刷新顺序不影响同键项目", () => {
  assert.equal(compareListLabels("A群", "阿群"), 0);
  assert.equal(compareListLabels("B群", "波群"), 0);
  assert.ok(compareListLabels("阿群", "B群") < 0);
  assert.ok(compareListLabels("波2群", "b10群") < 0);
  const items = [
    { id: 6, label: "C群" },
    { id: 5, label: "波群" },
    { id: 4, label: "B群" },
    { id: 3, label: "阿群" },
    { id: 2, label: "A群" },
    { id: 1, label: "2群" },
  ];
  const order = (source) =>
    sortListByLabel(
      source,
      (row) => row.label,
      (row) => row.id,
    );
  const result = order(items);
  assert.deepEqual(
    result.map((row) => row.label),
    ["2群", "A群", "阿群", "B群", "波群", "C群"],
  );
  assert.deepEqual(order([...items].reverse()), result);
});

test("空名称置后，比较忽略空白但保留原始显示值", () => {
  assert.deepEqual(sortedLabels(["", "  ", " b ", " a "]), [
    " a ",
    " b ",
    "",
    "  ",
  ]);
  assert.equal(compareListLabels("  a  ", "A"), 0);
});

test("排序不修改源数组或对象，同名稳定 ID 不依赖刷新返回顺序", () => {
  const items = Object.freeze([
    Object.freeze({ id: 10, name: "A" }),
    Object.freeze({ id: 2, name: "a" }),
    Object.freeze({ id: 1, name: "10" }),
  ]);
  const order = (source) =>
    sortListByLabel(
      source,
      (item) => item.name,
      (item) => item.id,
    );
  const sorted = order(items);
  assert.deepEqual(
    sorted.map((item) => item.id),
    [1, 2, 10],
  );
  assert.deepEqual(order([...items].reverse()), sorted);
  assert.deepEqual(
    items.map((item) => item.id),
    [10, 2, 1],
  );
  assert.equal(sorted[0], items[2]);
  assert.deepEqual(order([]), []);
});

test("完整集合先排序再分页、搜索，避免页间逆序", () => {
  const rows = Array.from({ length: 63 }, (_, i) => ({
    id: i + 1,
    name: String(63 - i),
  }));
  const sorted = sortListByLabel(
    rows,
    (item) => item.name,
    (item) => item.id,
  );
  assert.equal(sorted[0].name, "1");
  assert.equal(sorted[49].name, "50");
  assert.equal(sorted[50].name, "51");
  assert.deepEqual(
    sorted.filter((item) => item.name.includes("2")).map((item) => item.name),
    [
      "2",
      "12",
      "20",
      "21",
      "22",
      "23",
      "24",
      "25",
      "26",
      "27",
      "28",
      "29",
      "32",
      "42",
      "52",
      "62",
    ],
  );
});

test("混合字段比较保持反对称与传递性，防止浏览器排序结果不稳定", () => {
  const fields = [
    "",
    " ",
    "0",
    "00",
    "2",
    "02",
    "10",
    "a",
    "A",
    "a2",
    "a02",
    "a10",
    "B",
    "!",
    "阿",
    "啊",
    "八",
    "波",
    "李",
    "张",
    "張",
    "中",
    "\uE000",
    "😀",
    "𠀀",
  ];
  for (const a of fields) {
    for (const b of fields) {
      assert.equal(
        Math.sign(compareListLabels(a, b)) + Math.sign(compareListLabels(b, a)),
        0,
      );
      for (const c of fields) {
        if (compareListLabels(a, b) <= 0 && compareListLabels(b, c) <= 0)
          assert.ok(compareListLabels(a, c) <= 0, `${a}/${b}/${c}`);
      }
    }
  }
});
