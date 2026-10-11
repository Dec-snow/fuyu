// 用量图与费用显示的测试。
//
// 守的是「画出来的图不对但不报错」这一类问题：除零得到 NaN%、很小的值
// 画成 0 宽、费用显示成 ¥0（看起来像没花钱）。

import { describe, expect, it } from "vitest";

import type { UsageDay } from "../types";
import {
  barPercent,
  barWidth,
  formatCost,
  formatTokens,
  maxDayTokens,
  shortDay,
  totalTokens,
} from "./usage";

const day = (d: string, input: number, output: number): UsageDay => ({
  day: d,
  inputTokens: input,
  outputTokens: output,
  messages: 1,
  cost: 0,
  hasUnpriced: false,
});

describe("totalTokens / maxDayTokens", () => {
  it("一天的量是输入加输出", () => {
    expect(totalTokens(day("2026-10-11", 100, 25))).toBe(125);
  });

  it("取最大的一天；空数组是 0（不能是 -Infinity）", () => {
    expect(maxDayTokens([day("a", 1, 2), day("b", 10, 0)])).toBe(10);
    expect(maxDayTokens([])).toBe(0);
  });
});

describe("barPercent", () => {
  it("按最大值等比缩放", () => {
    expect(barPercent(50, 100)).toBe(50);
    expect(barPercent(100, 100)).toBe(100);
  });

  it("最大值为 0 时返回 0，而不是 NaN —— NaN 会被原样写进 style", () => {
    // 这是真发生过的坑：新装的用户没有任何用量，所有天都是 0，
    // 0/0 得到 NaN，React 会老老实实渲染成 width: NaN%。
    expect(barPercent(0, 0)).toBe(0);
    expect(barPercent(5, 0)).toBe(0);
    expect(Number.isNaN(barPercent(0, 0))).toBe(false);
  });

  it("不会超过 100%（数据异常时也别画出界）", () => {
    expect(barPercent(300, 100)).toBe(100);
  });

  it("负值/非有限值一律当 0", () => {
    expect(barPercent(-5, 100)).toBe(0);
    expect(barPercent(Infinity, 100)).toBe(0);
  });
});

describe("barWidth", () => {
  it("有量就至少占 1.5%，否则那天的柱子看起来像没有", () => {
    expect(barWidth(1, 100000)).toBe("1.5%");
  });

  it("正常情况就是比例", () => {
    expect(barWidth(50, 100)).toBe("50%");
  });

  it("没量就是 0%", () => {
    expect(barWidth(0, 100)).toBe("0%");
  });
});

describe("shortDay", () => {
  it("去掉年份", () => {
    expect(shortDay("2026-10-11")).toBe("10-11");
  });

  it("格式不对时原样返回，而不是崩掉或显示 undefined", () => {
    expect(shortDay("2026-10")).toBe("2026-10");
    expect(shortDay("")).toBe("");
  });
});

describe("formatTokens", () => {
  it("加千分位", () => {
    expect(formatTokens(1234567)).toBe("1,234,567");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(0)).toBe("0");
  });
});

describe("formatCost", () => {
  it("0 就是 ¥0", () => {
    expect(formatCost(0)).toBe("¥0");
  });

  it("小于一分钱显示 <¥0.01，而不是 ¥0.00（后者看起来像没花钱）", () => {
    expect(formatCost(0.0001)).toBe("<¥0.01");
    expect(formatCost(0.009)).toBe("<¥0.01");
  });

  it("一块钱以内保留两位，超过十块保留一位（避免「¥1234.5678」）", () => {
    expect(formatCost(0.05)).toBe("¥0.05");
    expect(formatCost(9.994)).toBe("¥9.99");
    expect(formatCost(12.3456)).toBe("¥12.3");
  });

  it("负值与非有限值都当 0 处理，不能显示成 ¥NaN", () => {
    expect(formatCost(-1)).toBe("¥0");
    expect(formatCost(NaN)).toBe("¥0");
    expect(formatCost(Infinity)).toBe("¥0");
  });
});
