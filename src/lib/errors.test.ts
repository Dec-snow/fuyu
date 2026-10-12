// describeError 的测试。
//
// 守的是「用户看到的错误信息有没有意义」：这个函数当初就是为了修掉
// 界面上那 24 处 `[object Object]` 才写的。

import { describe, expect, it } from "vitest";

import { describeError } from "./errors";

describe("describeError", () => {
  it("后端那种 {code, message} 结构：取 message", () => {
    expect(describeError({ code: "network", message: "连接超时" })).toBe("连接超时");
  });

  it("只有 code 时退回 code —— 至少让用户知道是哪一类失败", () => {
    expect(describeError({ code: "invalid_input" })).toBe("invalid_input");
  });

  it("Error 实例取 message", () => {
    expect(describeError(new Error("炸了"))).toBe("炸了");
  });

  it("字符串原样返回", () => {
    expect(describeError("网络不通")).toBe("网络不通");
  });

  it("认不出的对象退到 JSON，而不是 [object Object]", () => {
    const text = describeError({ weird: 1 });
    expect(text).not.toBe("[object Object]");
    expect(text).toContain("weird");
  });

  it("绝不留空：null / undefined / 空对象 / 空字符串都给一句兜底", () => {
    for (const value of [null, undefined, {}, "", "   ", 0, false]) {
      const text = describeError(value);
      expect(text.length, `${JSON.stringify(value)} 不该得到空字符串`).toBeGreaterThan(0);
      expect(text).not.toBe("[object Object]");
    }
  });

  it("循环引用不会让它自己崩掉", () => {
    const circular: Record<string, unknown> = { name: "环" };
    circular.self = circular;
    expect(() => describeError(circular)).not.toThrow();
    expect(describeError(circular).length).toBeGreaterThan(0);
  });

  it("空白 message 不算有效，继续往下找", () => {
    expect(describeError({ message: "   ", code: "timeout" })).toBe("timeout");
  });
});
