// 模型列表展示规则的测试。

import { describe, expect, it } from "vitest";

import type { ModelEntry } from "../types";
import { favoriteCount, modelLabel, sortModels } from "./models";

const m = (key: string, favorite = false): ModelEntry => ({ key, favorite });

describe("模型列表展示", () => {
  describe("sortModels", () => {
    it("收藏的排前面，其余保持原顺序", () => {
      const models = [m("a"), m("b", true), m("c"), m("d", true), m("e")];
      expect(sortModels(models).map((x) => x.key)).toEqual([
        "b",
        "d",
        "a",
        "c",
        "e",
      ]);
    });

    it("一个都没收藏时顺序完全不变", () => {
      const models = [m("x"), m("y"), m("z")];
      expect(sortModels(models).map((x) => x.key)).toEqual(["x", "y", "z"]);
    });

    it("不改动传进来的数组", () => {
      // 原地排序会改到 store 里的数组：引用没变，React 会认为「没变化」，
      // 于是收藏点了、界面却不动 —— 这类 bug 很难查。
      const models = [m("a"), m("b", true)];
      const sorted = sortModels(models);
      expect(models.map((x) => x.key)).toEqual(["a", "b"]);
      expect(sorted.map((x) => x.key)).toEqual(["b", "a"]);
      expect(sorted).not.toBe(models);
    });

    it("同一组内保持稳定（反复排序结果一致）", () => {
      const models = [m("a"), m("b"), m("c"), m("d")];
      const once = sortModels(models).map((x) => x.key);
      const twice = sortModels(sortModels(models)).map((x) => x.key);
      expect(twice).toEqual(once);
    });

    it("空列表不会炸", () => {
      expect(sortModels([])).toEqual([]);
    });
  });

  describe("modelLabel", () => {
    it("收藏的带星标前缀", () => {
      expect(modelLabel(m("deepseek-chat", true))).toBe("★ deepseek-chat");
      expect(modelLabel(m("deepseek-chat"))).toBe("deepseek-chat");
    });
  });

  describe("favoriteCount", () => {
    it("数得对", () => {
      expect(favoriteCount([m("a", true), m("b"), m("c", true)])).toBe(2);
      expect(favoriteCount([])).toBe(0);
    });
  });
});
