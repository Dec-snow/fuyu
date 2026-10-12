// 检查更新这条链路的显示逻辑（纯函数，可单测）。
//
// 单独写一个文件，是因为「要不要提示用户」这件事有好几种状态，而它们很容易
// 被写成一句 `if (hasUpdate)`：还没有发布过版本、已经是最新、用户跳过了这个版本、
// 检查失败 —— 四种情况该说的话完全不同，混在一起用户就会看到
// 「发现新版本 v0.1.0（当前 0.1.0）」这种自相矛盾的提示。

import type { UpdateCheck } from "../types";
import { formatBytes } from "./kb";

/** 这次检查该怎么向用户汇报。 */
export type UpdateStatus =
  /** 有新版，且用户没跳过 → 值得提示。 */
  | { kind: "available"; latest: string }
  /** 有新版但用户跳过过 → 只在设置页显示，不主动提示。 */
  | { kind: "skipped"; latest: string }
  /** 已经是最新。 */
  | { kind: "current" }
  /** 仓库还没有发布过任何版本。 */
  | { kind: "no-release" };

export function updateStatus(check: UpdateCheck): UpdateStatus {
  if (!check.hasUpdate) {
    return check.latest === null ? { kind: "no-release" } : { kind: "current" };
  }
  const latest = check.latest ?? "";
  return check.skipped ? { kind: "skipped", latest } : { kind: "available", latest };
}

/** 设置页里那一行结论。 */
export function describeUpdate(check: UpdateCheck): string {
  const status = updateStatus(check);
  switch (status.kind) {
    case "available":
      return `有新版本 ${status.latest}（当前 ${check.current}）`;
    case "skipped":
      return `有新版本 ${status.latest}，但你选择跳过（当前 ${check.current}）`;
    case "current":
      return `已经是最新版本（${check.current}）`;
    case "no-release":
      return `还没有发布过版本（当前 ${check.current}）`;
  }
}

/** 安装包那一行的说明；没有安装包时返回空串（不显示这一行）。 */
export function describeAsset(check: UpdateCheck): string {
  if (!check.assetName) return "";
  const size = check.assetSize ? ` · ${formatBytes(check.assetSize)}` : "";
  return `${check.assetName}${size}`;
}

/**
 * 值不值得主动弹提示（启动时那次检查用）。
 *
 * 只有「有新版且没被跳过」才提示。「已是最新」提示一次是骚扰，
 * 「仓库没发过版本」更不该提示 —— 用户会以为是自己哪里配错了。
 */
export function shouldNotify(check: UpdateCheck): boolean {
  return updateStatus(check).kind === "available";
}
