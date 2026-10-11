// 复制功能的测试。
//
// 守的是「静默失败」这一类问题：复制最大的坑不是复制不了，而是**看起来复制成功了**
// —— 失败被吞掉、按钮照样显示「已复制」，用户粘出来还是上一次的内容。
// 所以这里重点覆盖「失败必须变成一个能看见的结果」。

import { beforeEach, describe, expect, it, vi } from "vitest";

// `vi.mock` 的工厂会被提升到文件顶部执行，那时普通 `const` 还在 TDZ 里
// （会报 `Cannot access 'api' before initialization`）。
// `vi.hoisted` 就是为这个场景准备的：它保证这段初始化先于 import 求值。
const { api } = vi.hoisted(() => ({
  api: { copyToClipboard: vi.fn() },
}));

vi.mock("../services/api", () => ({ api }));

import { canCopy, copyText } from "./clipboard";

describe("复制到剪贴板", () => {
  beforeEach(() => {
    api.copyToClipboard.mockReset();
  });

  describe("canCopy：哪些内容才给复制入口", () => {
    it("空白内容不给", () => {
      expect(canCopy("")).toBe(false);
      expect(canCopy("   ")).toBe(false);
      expect(canCopy("\n\t ")).toBe(false);
      expect(canCopy(null)).toBe(false);
      expect(canCopy(undefined)).toBe(false);
    });

    it("有实际内容就给（首尾空白不影响判断）", () => {
      expect(canCopy("你好")).toBe(true);
      expect(canCopy("  内容  ")).toBe(true);
      expect(canCopy("```\ncode\n```")).toBe(true);
    });
  });

  describe("copyText", () => {
    it("空内容返回 empty，且不打扰后端", async () => {
      await expect(copyText("   ")).resolves.toBe("empty");
      expect(api.copyToClipboard).not.toHaveBeenCalled();
    });

    it("成功返回 copied，并把原文一字不改地交给后端", async () => {
      api.copyToClipboard.mockResolvedValue(undefined);
      const text = "  前后都有空格\n第二行  ";
      await expect(copyText(text)).resolves.toBe("copied");
      expect(api.copyToClipboard).toHaveBeenCalledWith(text);
    });

    it("后端失败时返回 failed，而不是把异常抛出去", async () => {
      // 抛出去的话，调用点一个 `void copy()` 就把失败吞了 ——
      // 那正是「以为复制成功」的来源。
      api.copyToClipboard.mockRejectedValue(new Error("剪贴板被别的程序占住"));
      await expect(copyText("你好")).resolves.toBe("failed");
    });

    it("失败之后还能再成功（状态不是一次性的）", async () => {
      api.copyToClipboard.mockRejectedValueOnce(new Error("busy"));
      await expect(copyText("你好")).resolves.toBe("failed");
      api.copyToClipboard.mockResolvedValueOnce(undefined);
      await expect(copyText("你好")).resolves.toBe("copied");
    });
  });
});
