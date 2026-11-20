/** 非敏感偏好边界回归；使用内存存储，绝不读取真实用户配置。@author ahui */
import assert from "node:assert/strict";
import { test, beforeEach } from "node:test";
import {
  readMailOptions,
  saveMailOptions,
  readListFilter,
  saveListFilter,
  DEFAULT_MAIL_OPTIONS,
} from "../lib/preferences.ts";

let storage;
beforeEach(() => {
  storage = new Map();
  globalThis.window = {
    localStorage: {
      getItem: (key) => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
    },
  };
});

test("缺省、坏JSON与未知版本恢复原有默认，个别坏字段不丢弃其他合法选择", () => {
  assert.deepEqual(readMailOptions(), DEFAULT_MAIL_OPTIONS);
  for (const value of [
    "{",
    "null",
    "[]",
    '{"version":2,"intervalSeconds":1}',
  ]) {
    storage.set("qq-group-exporter-mail-options-v1", value);
    assert.deepEqual(readMailOptions(), DEFAULT_MAIL_OPTIONS);
  }
  storage.set(
    "qq-group-exporter-mail-options-v1",
    JSON.stringify({
      version: 1,
      skipSent: false,
      sendMode: "invalid",
      batchSize: -1,
      intervalSeconds: 300,
      open: true,
    }),
  );
  assert.deepEqual(readMailOptions(), {
    ...DEFAULT_MAIL_OPTIONS,
    skipSent: false,
    intervalSeconds: 300,
    open: true,
  });
});

test("所有发送选项可恢复，空值小数和越界输入保留上次合法值，明确0才能关闭切换", () => {
  const saved = {
    ...DEFAULT_MAIL_OPTIONS,
    skipSent: false,
    continueOnFailure: true,
    sendMode: "individual",
    batchSize: 23,
    intervalSeconds: 800,
    accountSwitchIntervalSeconds: 45,
    open: true,
  };
  assert.equal(saveMailOptions(saved), true);
  assert.deepEqual(readMailOptions(), saved);
  saveMailOptions({
    ...saved,
    batchSize: 0,
    intervalSeconds: 3601,
    accountSwitchIntervalSeconds: NaN,
  });
  assert.deepEqual(readMailOptions(), saved);
  saveMailOptions({
    ...saved,
    batchSize: 1.5,
    intervalSeconds: NaN,
    accountSwitchIntervalSeconds: 0,
  });
  assert.deepEqual(readMailOptions(), {
    ...saved,
    accountSwitchIntervalSeconds: 0,
  });
});

test("名单筛选按登录来源及QQ隔离，取消好友和清空排除群也能保存", () => {
  const selected = {
    excludeSent: false,
    excludeFriends: true,
    excludeIds: ["900002"],
    open: true,
  };
  saveListFilter("web:10001", selected);
  assert.deepEqual(readListFilter("web:10001"), selected);
  assert.deepEqual(readListFilter("web:10002"), readListFilter(""));
  assert.deepEqual(readListFilter("local:10001"), readListFilter(""));
  saveListFilter("web:10001", {
    ...selected,
    excludeFriends: false,
    excludeIds: [],
  });
  assert.deepEqual(readListFilter("web:10001"), {
    ...selected,
    excludeFriends: false,
    excludeIds: [],
  });
  assert.equal(saveListFilter("", selected), false);
});

test("排除标识去重并剔除异常类型，未知字段及敏感内容不会回写", () => {
  storage.set(
    "qq-group-exporter-list-filter-v1:web%3A10001",
    JSON.stringify({
      version: 1,
      excludeIds: ["900002", "900002", null, 123, "bad", "123\n"],
      excludeFriends: "true",
    }),
  );
  assert.deepEqual(readListFilter("web:10001").excludeIds, ["900002"]);
  assert.equal(readListFilter("web:10001").excludeFriends, false);
  saveMailOptions({
    ...DEFAULT_MAIL_OPTIONS,
    html: "private-body",
    password: "private-password",
    recipients: "private-address",
  });
  assert.doesNotMatch(
    storage.get("qq-group-exporter-mail-options-v1"),
    /private/,
  );
});

test("存储被拒绝时读取可回退、写入报告失败，不抛错阻断业务", () => {
  Object.defineProperty(window, "localStorage", {
    get() {
      throw new Error("denied");
    },
  });
  assert.deepEqual(readMailOptions(), DEFAULT_MAIL_OPTIONS);
  assert.equal(saveMailOptions(DEFAULT_MAIL_OPTIONS), false);
  assert.deepEqual(readListFilter("web:10001"), readListFilter(""));
  assert.equal(saveListFilter("web:10001", readListFilter("")), false);
});
