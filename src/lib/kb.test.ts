// 知识库界面辅助函数的测试。
//
// 守的是「显示出来的东西误导人」这一类问题：把 0 篇文档显示成一排 0、
// 预览悄悄截断却不给省略号、认不出类型却放行。

import { describe, expect, it } from "vitest";

import type { KbDocument, KbHit, KbStats } from "../types";
import {
  describeDocument,
  describeHit,
  describeStats,
  formatChars,
  kbFileKind,
  previewText,
  titleFromFileName,
} from "./kb";

const doc = (over: Partial<KbDocument> = {}): KbDocument => ({
  id: "d1",
  title: "浮语手册",
  source: "manual.md",
  chars: 8400,
  chunkCount: 12,
  addedAt: 0,
  ...over,
});

const stats = (over: Partial<KbStats> = {}): KbStats => ({
  documents: 2,
  chunks: 24,
  terms: 1500,
  chars: 16800,
  ...over,
});

describe("formatChars", () => {
  it("按量级换单位", () => {
    expect(formatChars(0)).toBe("0");
    expect(formatChars(999)).toBe("999");
    expect(formatChars(8400)).toBe("8.4 千");
    expect(formatChars(2_500_000)).toBe("2.5 百万");
  });
});

describe("describeDocument", () => {
  it("带上来源、片数与字数", () => {
    expect(describeDocument(doc())).toBe("manual.md · 12 片 · 8.4 千 字");
  });

  it("来源为空时不留下一个孤零零的分隔符", () => {
    const line = describeDocument(doc({ source: "   " }));
    expect(line.startsWith("·")).toBe(false);
    expect(line).toContain("12 片");
  });
});

describe("describeStats", () => {
  it("空库给一句人话，而不是一排 0", () => {
    expect(describeStats(null)).toContain("还是空的");
    expect(describeStats(stats({ documents: 0 }))).toContain("还是空的");
  });

  it("有内容时报出规模", () => {
    const line = describeStats(stats());
    expect(line).toContain("2 篇");
    expect(line).toContain("24 片");
    expect(line).toContain("1.5 千");
  });
});

describe("describeHit", () => {
  it("带上片号，用户才能回去核对", () => {
    const hit = { title: "浮语手册", ordinal: 3 } as KbHit;
    expect(describeHit(hit)).toBe("浮语手册 · 第 3 片");
  });
});

describe("previewText", () => {
  it("把换行压成空格", () => {
    expect(previewText("第一行\n第二行")).toBe("第一行 第二行");
  });

  it("超长时截断并**明确**加省略号", () => {
    const long = "啊".repeat(200);
    const out = previewText(long, 50);
    expect(out.length).toBe(51); // 50 + 省略号
    expect(out.endsWith("…")).toBe(true);
  });

  it("刚好不超时不加省略号", () => {
    const exact = "啊".repeat(50);
    expect(previewText(exact, 50)).toBe(exact);
  });

  it("空文本不炸", () => {
    expect(previewText("")).toBe("");
    expect(previewText("   \n  ")).toBe("");
  });
});

describe("kbFileKind", () => {
  it("Office 走解析、文本走读取", () => {
    expect(kbFileKind("报告.docx")).toBe("office");
    expect(kbFileKind("数据.XLSX")).toBe("office");
    expect(kbFileKind("笔记.md")).toBe("text");
    expect(kbFileKind("code.rs")).toBe("text");
  });

  it("不认识的类型返回 null（调用方据此提示，而不是硬塞进去）", () => {
    for (const name of ["a.pdf", "a.exe", "a.png", "无扩展名", "结尾是点."]) {
      expect(kbFileKind(name), name).toBeNull();
    }
  });
});

describe("titleFromFileName", () => {
  it("去掉扩展名", () => {
    expect(titleFromFileName("季度报告.docx")).toBe("季度报告");
    expect(titleFromFileName("no-ext")).toBe("no-ext");
  });
});
