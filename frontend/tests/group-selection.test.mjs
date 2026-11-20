/** 群组合稳定键和来源展示边界；不读取任何真实名单。@author ahui */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  groupSelectionKey,
  snapshotGroupIds,
  snapshotGroupLabel,
} from "../lib/group-selection.ts";

test("群组合顺序无关、重复合并，单群和空选择兼容", () => {
  const ids = ["900002", "900001", "900002"];
  assert.equal(groupSelectionKey(ids), "multi:900001,900002");
  assert.deepEqual(ids, ["900002", "900001", "900002"]);
  assert.equal(groupSelectionKey([]), "");
  assert.equal(groupSelectionKey(["900001"]), "900001");
});
test("多群展示只使用实际来源，单群旧JSON继续可用", () => {
  const single = { group: { id: "900001", name: "甲群" } };
  assert.deepEqual(snapshotGroupIds(single), ["900001"]);
  assert.equal(snapshotGroupLabel(single), "群号 900001");
  const multiple = {
    group: { id: "multi:900001,900002", name: "2个群聊" },
    sourceGroups: [
      { group: { id: "900001", name: "甲群" } },
      { group: { id: "900002", name: "乙群" } },
    ],
  };
  assert.deepEqual(snapshotGroupIds(multiple), ["900001", "900002"]);
  assert.equal(snapshotGroupLabel(multiple), "甲群（900001）、乙群（900002）");
  assert.doesNotMatch(snapshotGroupLabel(multiple), /multi:/);
});
