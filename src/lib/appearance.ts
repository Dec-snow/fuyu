// 外观相关的取值范围与换算（纯函数，可单测）。
//
// ⚠️ 这里的常量**必须和 Rust 侧保持一致**：
// `src-tauri/src/storage/config.rs` 里的 `MIN_WINDOW_OPACITY` / `MIN_FONT_SIZE` /
// `MAX_FONT_SIZE` / `DEFAULT_FONT_SIZE`。
//
// 为什么两边都要有：后端是**权威**（用户可以直接改 providers.json，或者调用
// 命令），前端这份只用来决定滑块范围、以及万一拿到越界值时别把界面搞坏。
// 两边不一致的后果很具体：滑块能拖到 0，界面当场全透明。

/** 窗口背景不透明度下限（与 Rust `MIN_WINDOW_OPACITY` 一致）。 */
export const MIN_WINDOW_OPACITY = 0.35;
/** 正文字号范围（与 Rust `MIN_FONT_SIZE` / `MAX_FONT_SIZE` 一致）。 */
export const MIN_FONT_SIZE = 11;
export const MAX_FONT_SIZE = 18;
/** 默认字号，与 CSS 里 `body` 原本的字号一致。 */
export const DEFAULT_FONT_SIZE = 13.5;

/** 滑块用百分比（整数更好拖），存储用 0~1 的小数。 */
export function opacityToPercent(opacity: number): number {
  return Math.round(clampOpacity(opacity) * 100);
}

export function opacityFromPercent(percent: number): number {
  const safe = Number.isFinite(percent) ? percent : 100;
  return clampOpacity(safe / 100);
}

/** 把不透明度收进合法区间；非有限值退回 1（完全可见）。 */
export function clampOpacity(value: number): number {
  if (!Number.isFinite(value)) return 1;
  return Math.min(1, Math.max(MIN_WINDOW_OPACITY, value));
}

/** 把字号收进合法区间；非有限值退回默认值。 */
export function clampFontSize(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_FONT_SIZE;
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, value));
}
