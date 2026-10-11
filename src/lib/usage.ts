// 用量图与费用显示用的纯函数（可单测）。
//
// 单独拎出来是因为这里有**两个会画出坏图**的地方：
// - 全是 0 的时候做除数 → `NaN%`，React 会老老实实把它写进 style，
//   于是整条柱子消失或宽度异常；
// - 「有量但很小」的那天如果按比例画就只有不到一个像素，看起来像这天没用量。
// 两者都不报错，只是图不对。

import type { UsageDay } from "../types";

/** 一天的总 token（输入 + 输出）。 */
export function totalTokens(day: UsageDay): number {
  return day.inputTokens + day.outputTokens;
}

/** 所有天里最大的一天总 token；空数组返回 0。 */
export function maxDayTokens(days: UsageDay[]): number {
  return days.reduce((max, d) => Math.max(max, totalTokens(d)), 0);
}

/** 一天该画多宽（百分比，0~100）。 */
export function barPercent(value: number, max: number): number {
  // 非有限值一律当 0：`Infinity` 若只做 `Math.min(100, …)` 会得到**满格**，
  // 于是一个坏数据反而画成「这天用量最大」。NaN 也会被原样写进 style。
  if (!Number.isFinite(value) || !Number.isFinite(max)) return 0;
  // 全 0（或还没数据）时不能做除法。
  if (value <= 0 || max <= 0) return 0;
  return Math.min(100, (value / max) * 100);
}

/** 有量但很小时给一个最小可见宽度 —— 否则那天的柱子看起来是空的。 */
export function barWidth(value: number, max: number): string {
  const percent = barPercent(value, max);
  if (percent === 0) return "0%";
  return `${Math.max(percent, 1.5)}%`;
}

/** `2026-10-11` → `10-11`（图上不需要年份）。 */
export function shortDay(day: string): string {
  const parts = day.split("-");
  return parts.length === 3 ? `${parts[1]}-${parts[2]}` : day;
}

/** 大数字加千分位，读起来省力。 */
export function formatTokens(n: number): string {
  return n.toLocaleString("en-US");
}

/**
 * 费用（元）。
 *
 * 小于一分钱时显示 `<¥0.01` 而不是 `¥0.00`：显示成 0 会让人以为「没花钱」，
 * 而实际上只是这一段时间太短。这类「看起来是 0」的数字最容易误导。
 */
export function formatCost(yuan: number): string {
  if (!Number.isFinite(yuan) || yuan <= 0) return "¥0";
  if (yuan < 0.01) return "<¥0.01";
  if (yuan < 10) return `¥${yuan.toFixed(2)}`;
  return `¥${yuan.toFixed(1)}`;
}
