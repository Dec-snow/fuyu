// 更新提示的显示逻辑测试。
//
// 守的是「四种状态被混成一句 if」这一类问题：那会让用户看到
// 「发现新版本 v0.1.0（当前 0.1.0）」这种自相矛盾的提示。

import { describe, expect, it } from "vitest";

import type { UpdateCheck } from "../types";
import { formatBytes } from "./kb";
import {
  describeAsset,
  describeUpdate,
  shouldNotify,
  updateStatus,
} from "./update";

const check = (over: Partial<UpdateCheck> = {}): UpdateCheck => ({
  current: "0.1.0",
  latest: "0.1.0",
  hasUpdate: false,
  skipped: false,
  notes: "",
  pageUrl: "https://github.com/Dec-snow/fuyu/releases/tag/v0.1.0",
  publishedAt: "2025-01-01T00:00:00Z",
  assetName: null,
  assetSize: null,
  checkedAt: 0,
  ...over,
});

describe("updateStatus", () => {
  it("区分四种状态", () => {
    expect(updateStatus(check({ latest: "0.2.0", hasUpdate: true })).kind).toBe(
      "available",
    );
    expect(
      updateStatus(check({ latest: "0.2.0", hasUpdate: true, skipped: true })).kind,
    ).toBe("skipped");
    // 仓库还没有发布过版本：latest 是 null
    expect(updateStatus(check({ latest: null, hasUpdate: false })).kind).toBe(
      "no-release",
    );
    expect(updateStatus(check({ hasUpdate: false })).kind).toBe("current");
  });
});

describe("describeUpdate", () => {
  it("有新版时报出新旧两个版本号", () => {
    const line = describeUpdate(check({ latest: "0.2.0", hasUpdate: true }));
    expect(line).toContain("0.2.0");
    expect(line).toContain("0.1.0");
  });

  it("已经是最新时说「已经是最新」，而不是报一个同名的新版本", () => {
    // 这是最容易写错的一句：`hasUpdate` 为假时不能还说「有新版本」
    const line = describeUpdate(check());
    expect(line).toContain("已经是最新");
    expect(line).not.toContain("有新版本");
  });

  it("跳过过的版本要说明是被跳过的", () => {
    const line = describeUpdate(
      check({ latest: "0.2.0", hasUpdate: true, skipped: true }),
    );
    expect(line).toContain("跳过");
  });

  it("没发布过版本时说清楚，免得用户以为是自己配错了", () => {
    const line = describeUpdate(check({ latest: null }));
    expect(line).toContain("还没有发布过版本");
  });
});

describe("shouldNotify", () => {
  it("只有「有新版且没跳过」才值得主动弹提示", () => {
    expect(shouldNotify(check({ latest: "0.2.0", hasUpdate: true }))).toBe(true);
    // 「已是最新」弹一次是骚扰
    expect(shouldNotify(check())).toBe(false);
    // 跳过过的就别再弹了（否则「跳过」这个按钮等于没有）
    expect(
      shouldNotify(check({ latest: "0.2.0", hasUpdate: true, skipped: true })),
    ).toBe(false);
    // 仓库没发过版本更不该提示
    expect(shouldNotify(check({ latest: null }))).toBe(false);
  });
});

describe("describeAsset", () => {
  it("带出安装包名字与大小", () => {
    const line = describeAsset(
      check({ assetName: "fuyu_0.2.0_x64-setup.exe", assetSize: 12345678 }),
    );
    expect(line).toContain("fuyu_0.2.0_x64-setup.exe");
    expect(line).toContain("11.8 MB");
  });

  it("没有安装包时给空串（界面据此不显示这一行）", () => {
    expect(describeAsset(check())).toBe("");
  });

  it("有名字没大小时不留一个孤零零的分隔符", () => {
    const line = describeAsset(check({ assetName: "x.exe" }));
    expect(line).toBe("x.exe");
  });
});

describe("formatBytes", () => {
  it("按量级换单位", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2 KB");
    expect(formatBytes(12345678)).toBe("11.8 MB");
  });

  it("0 或非有限值给「未知大小」，而不是 0 B", () => {
    // 「0 B 的安装包」看起来像出错了
    expect(formatBytes(0)).toBe("未知大小");
    expect(formatBytes(Number.NaN)).toBe("未知大小");
    expect(formatBytes(-1)).toBe("未知大小");
  });
});
