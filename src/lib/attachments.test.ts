// 附件校验与拼装的测试。
//
// 这里守的都是「不会报错、但结果不对」的输入：
// 改了扩展名的二进制、不是 UTF-8 的文本、本身带 ``` 的 Markdown。

import { describe, expect, it } from "vitest";

import {
  attachmentBlock,
  buildMessage,
  canAddAttachment,
  decodeText,
  extensionOf,
  fenceFor,
  isAllowedExtension,
  looksBinary,
  MAX_ATTACHMENT_BYTES,
  MAX_ATTACHMENTS,
  type Attachment,
} from "./attachments";

const enc = (s: string) => new TextEncoder().encode(s);
const att = (name: string, text: string): Attachment => ({
  name,
  text,
  bytes: enc(text).length,
});

describe("extensionOf / isAllowedExtension", () => {
  it("取出小写扩展名", () => {
    expect(extensionOf("notes.md")).toBe("md");
    expect(extensionOf("A.RS")).toBe("rs");
    expect(extensionOf("archive.tar.gz")).toBe("gz");
  });

  it("没有扩展名 / 隐藏文件 / 结尾是点的情况", () => {
    expect(extensionOf("README")).toBe("");
    expect(extensionOf(".gitignore")).toBe(""); // 开头的点不算扩展名
    expect(extensionOf("weird.")).toBe("");
  });

  it("白名单：接受文本与代码", () => {
    for (const n of ["a.txt", "a.md", "a.json", "a.yaml", "a.rs", "a.tsx", "a.py", "a.sql"]) {
      expect(isAllowedExtension(n), n).toBe(true);
    }
  });

  it("白名单：拒绝可执行文件、图片、压缩包等", () => {
    for (const n of ["a.exe", "a.dll", "a.png", "a.jpg", "a.zip", "a.pdf", "a.docx", "a.db"]) {
      expect(isAllowedExtension(n), n).toBe(false);
    }
  });
});

describe("looksBinary", () => {
  it("普通文本（含中文与 emoji）不是二进制", () => {
    expect(looksBinary(enc("你好，世界 🌊\n第二行\t带制表符"))).toBe(false);
  });

  it("空内容不算二进制（空文件是合法的）", () => {
    expect(looksBinary(new Uint8Array(0))).toBe(false);
  });

  it("出现 NUL 字节就是二进制 —— 这是把 .exe 改名成 .txt 的主要特征", () => {
    const bytes = new Uint8Array([0x4d, 0x5a, 0x00, 0x01, 0x02]); // "MZ\0.."
    expect(looksBinary(bytes)).toBe(true);
  });

  it("控制字符比例过高也算二进制（没有 NUL 也要挡）", () => {
    const bytes = new Uint8Array(100).fill(0x07); // 全是 BEL
    expect(looksBinary(bytes)).toBe(true);
  });

  it("少量控制字符放行（不误杀）", () => {
    const bytes = new Uint8Array(100).fill(0x61); // 'a'
    bytes[0] = 0x0c; // 一个换页符
    expect(looksBinary(bytes)).toBe(false);
  });
});

describe("decodeText", () => {
  it("合法 UTF-8 正常解码", () => {
    expect(decodeText(enc("中文 abc"))).toBe("中文 abc");
  });

  it("不是合法 UTF-8 时返回 null，而不是一堆 U+FFFD", () => {
    // 一段 GBK 编码的「中文」字节，在 UTF-8 下是非法的
    const gbk = new Uint8Array([0xd6, 0xd0, 0xce, 0xc4]);
    expect(decodeText(gbk)).toBeNull();
  });
});

describe("canAddAttachment", () => {
  it("正常文件放行", () => {
    expect(canAddAttachment([], "a.md", 1000)).toEqual({ ok: true });
  });

  it("拒绝不支持的扩展名，并说清是哪一类", () => {
    const r = canAddAttachment([], "a.exe", 10);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain(".exe");
  });

  it("没有扩展名也给一句人话，而不是空的类型名", () => {
    const r = canAddAttachment([], "README", 10);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain("无法识别");
  });

  it("拒绝超大的单个文件（上限 256 KB）", () => {
    const r = canAddAttachment([], "big.log", MAX_ATTACHMENT_BYTES + 1);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain("太大");
    expect(canAddAttachment([], "ok.log", MAX_ATTACHMENT_BYTES).ok).toBe(true);
  });

  it("拒绝超过个数上限", () => {
    const many = Array.from({ length: MAX_ATTACHMENTS }, (_, i) =>
      att(`f${i}.md`, "x"),
    );
    const r = canAddAttachment(many, "one-more.md", 1);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain("最多");
  });

  it("拒绝合计超过上限（3 个 200 KB 的文件）", () => {
    const each = 200 * 1024;
    const existing = [att("a.md", "x"), att("b.md", "x")];
    // 手工构造 bytes，避免真的造 400 KB 字符串
    existing[0].bytes = each;
    existing[1].bytes = each;
    const r = canAddAttachment(existing, "c.md", each);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain("合计");
    // 边界两侧都要验：合计 500 KB 可以，550 KB 不行。
    // （注意不能拿 MAX_TOTAL_ATTACHMENT_BYTES 当单个文件的大小 —— 它 512 KB，
    //   会先被 256 KB 的**单文件**上限挡掉，那样测的就不是合计这条规则了。）
    expect(canAddAttachment(existing, "d.md", 100 * 1024).ok).toBe(true);
    expect(canAddAttachment(existing, "e.md", 150 * 1024).ok).toBe(false);
  });
});

describe("fenceFor", () => {
  it("普通内容用三个反引号", () => {
    expect(fenceFor("hello")).toBe("```");
  });

  it("内容里有 ``` 时围栏加长 —— 否则会被内容提前闭合", () => {
    // 这是真正要防的东西：一篇讲 Markdown 的文档本身就有代码围栏，
    // 用固定三个反引号会让后面的内容跑到围栏外面，被当成对话正文。
    expect(fenceFor("```\ncode\n```")).toBe("````");
    expect(fenceFor("`````")).toBe("``````");
  });
});

describe("attachmentBlock", () => {
  it("带上文件名与语言标注，内容原样保留", () => {
    const block = attachmentBlock(att("main.rs", "fn main() {}"));
    expect(block).toContain("【附件：main.rs】");
    expect(block).toContain("```rs");
    expect(block).toContain("fn main() {}");
  });

  it("内容是 Markdown（自带围栏）时不会被提前闭合", () => {
    const inner = "# 标题\n\n```js\nconsole.log(1)\n```\n结束";
    const block = attachmentBlock(att("doc.md", inner));
    const fence = fenceFor(inner);
    // 收尾必须是那条更长的围栏，且内容原样在里面
    expect(block.endsWith(fence)).toBe(true);
    expect(block).toContain(inner);
    // 内容里那三个反引号不可能是收尾围栏
    expect(block.length).toBeGreaterThan(inner.length);
  });
});

describe("buildMessage", () => {
  it("没有附件时就是原文", () => {
    expect(buildMessage("你好", [])).toBe("你好");
  });

  it("附件在前、提问在后", () => {
    const msg = buildMessage("这段代码有问题吗？", [att("a.rs", "fn f() {}")]);
    expect(msg.indexOf("【附件：a.rs】")).toBeLessThan(msg.indexOf("这段代码有问题吗？"));
  });

  it("只有附件、没有文字时也成立（不能留下一个空段落）", () => {
    const msg = buildMessage("   ", [att("a.rs", "x")]);
    expect(msg.startsWith("【附件：a.rs】")).toBe(true);
    expect(msg.endsWith("\n\n")).toBe(false);
  });

  it("多个附件都带上", () => {
    const msg = buildMessage("看这两个", [att("a.rs", "1"), att("b.py", "2")]);
    expect(msg).toContain("【附件：a.rs】");
    expect(msg).toContain("【附件：b.py】");
  });
});
