// 模型列表的展示规则（纯函数，可单测）。
//
// 抽出来是因为「收藏排前面」这条规则有**两个地方**要用：输入框上方的模型选择器、
// 以及设置里的模型标签列表。如果各写一遍，早晚会出现「下拉框里按收藏排了、
// 设置里没排」这种不一致。

import type { ModelEntry } from "../types";

/**
 * 下拉框 / 列表里的模型顺序：**收藏的排前面**，其余保持原顺序。
 *
 * 两个细节：
 * - 用 `slice()` 先复制：`Array.prototype.sort` 是原地排序，直接排会把 store 里
 *   的数组也改掉（React 会因为「引用没变」而漏掉重渲染，界面看起来就是没反应）；
 * - 依赖排序**稳定性**（ES2019 起 `sort` 保证稳定）：同一组内保持服务商返回的
 *   原始顺序，否则用户会发现「每次打开下拉框顺序都不一样」。
 */
export function sortModels(models: ModelEntry[]): ModelEntry[] {
  return models.slice().sort((a, b) => Number(b.favorite) - Number(a.favorite));
}

/**
 * 模型在下拉框里的显示文案。
 *
 * 原生 `<select>` 里放不了按钮，所以「收藏」只能用文字前缀来表达 ——
 * 真正的星标开关在「设置 → 模型」的模型标签上。
 */
export function modelLabel(model: ModelEntry): string {
  return model.favorite ? `★ ${model.key}` : model.key;
}

/** 收藏了几个（用来在设置里给一句反馈，也方便测试断言）。 */
export function favoriteCount(models: ModelEntry[]): number {
  return models.filter((m) => m.favorite).length;
}
