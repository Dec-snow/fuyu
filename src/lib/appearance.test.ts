// 外观取值的测试。
//
// 这一组守的是「滑块能把应用弄坏」这类问题：不透明度写 0 → 窗口全透明；
// 字号写成 NaN → CSS 变量整条失效。两种情况的共同点是**用户没法自救**
// （看不见界面就点不到设置），所以必须在数据层就挡住。

import { describe, expect, it } from "vitest";

import {
  clampFontSize,
  clampOpacity,
  DEFAULT_FONT_SIZE,
  MAX_FONT_SIZE,
  MIN_FONT_SIZE,
  MIN_WINDOW_OPACITY,
  opacityFromPercent,
  opacityToPercent,
} from "./appearance";

describe("窗口不透明度", () => {
  it("区间内的值原样通过", () => {
    expect(clampOpacity(1)).toBe(1);
    expect(clampOpacity(0.8)).toBe(0.8);
    expect(clampOpacity(MIN_WINDOW_OPACITY)).toBe(MIN_WINDOW_OPACITY);
  });

  it("0 与负数被抬到下限（否则窗口彻底看不见）", () => {
    expect(clampOpacity(0)).toBe(MIN_WINDOW_OPACITY);
    expect(clampOpacity(-1)).toBe(MIN_WINDOW_OPACITY);
  });

  it("大于 1 被压到 1", () => {
    expect(clampOpacity(1.5)).toBe(1);
  });

  it("NaN / Infinity 退回 1（可见），而不是继续传下去", () => {
    expect(clampOpacity(Number.NaN)).toBe(1);
    expect(clampOpacity(Number.POSITIVE_INFINITY)).toBe(1);
  });

  it("百分比与小数互转是一一对应的", () => {
    expect(opacityToPercent(1)).toBe(100);
    expect(opacityToPercent(0.8)).toBe(80);
    expect(opacityFromPercent(80)).toBeCloseTo(0.8, 10);
    expect(opacityFromPercent(100)).toBe(1);
  });

  it("百分比越界也会被收进区间", () => {
    expect(opacityFromPercent(0)).toBe(MIN_WINDOW_OPACITY);
    expect(opacityFromPercent(999)).toBe(1);
    expect(opacityFromPercent(Number.NaN)).toBe(1);
  });
});

describe("正文字号", () => {
  it("区间内的值原样通过", () => {
    expect(clampFontSize(15)).toBe(15);
    expect(clampFontSize(MIN_FONT_SIZE)).toBe(MIN_FONT_SIZE);
    expect(clampFontSize(MAX_FONT_SIZE)).toBe(MAX_FONT_SIZE);
  });

  it("过小 / 过大被收进区间", () => {
    expect(clampFontSize(1)).toBe(MIN_FONT_SIZE);
    expect(clampFontSize(100)).toBe(MAX_FONT_SIZE);
  });

  it("非有限值退回默认字号", () => {
    expect(clampFontSize(Number.NaN)).toBe(DEFAULT_FONT_SIZE);
  });

  it("默认字号本身在区间内（否则默认配置就是非法的）", () => {
    expect(DEFAULT_FONT_SIZE).toBeGreaterThanOrEqual(MIN_FONT_SIZE);
    expect(DEFAULT_FONT_SIZE).toBeLessThanOrEqual(MAX_FONT_SIZE);
  });
});
