// 复制按钮的**渲染级**测试。
//
// 为什么用 `react-dom/server` 而不是 @testing-library/react：这个项目刻意不引入
// 那套依赖（jsdom + 一堆 matcher），而这两个组件要验证的东西只有「DOM 里渲染出来
// 的是什么」—— 服务端渲染就够，更快，而且更接近事实本身。
//
// ⚠️ 两个写这类测试必踩的坑，都踩过了：
// 1. **JSX 属性里的 `\n` 是字面量**，不是换行（`text="   \n "` 会得到一个带反斜杠的
//    字符串，`trim()` 之后长度是 2，于是"空内容"根本不空）。要真换行就用
//    JS 表达式或 `String.fromCharCode(10)`。
// 2. **不要在这里渲染读 zustand store 的组件**：SSR 下 zustand 走
//    `getServerSnapshot`，拿到的始终是**初始**状态（实测：`setState` 之后
//    `getState()` 有数据，组件里读到的仍是 null）。要测那种组件，得把 store
//    整个 mock 掉（见 ModelPicker.test.tsx）。

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { CopyButton } from "./CopyButton";
import { Markdown } from "./Markdown";

/** 真的换行（JSX 属性里写 `\n` 只会得到两个字面字符）。 */
const NEWLINE = String.fromCharCode(10);

describe("CopyButton", () => {
  it("空内容不渲染任何东西", () => {
    expect(renderToStaticMarkup(<CopyButton text="" />)).toBe("");
    expect(renderToStaticMarkup(<CopyButton text={`   ${NEWLINE} `} />)).toBe("");
  });

  it("有内容时渲染成真正的按钮（键盘也能用）", () => {
    const html = renderToStaticMarkup(<CopyButton text="你好" title="复制这条消息" />);
    expect(html).toContain("<button");
    expect(html).toContain("复制");
    // 必须是 button 而不是可点的 div —— 气泡上那两个按钮平时是 opacity:0，
    // 用 div 的话 Tab 聚焦不到，键盘用户就没有复制入口。
    expect(html).not.toContain("<div");
    // 屏幕上只显示两个字的按钮尤其需要无障碍标签。
    expect(html).toContain('aria-label="复制这条消息"');
  });

  it("自定义文案会体现在渲染结果里", () => {
    const html = renderToStaticMarkup(
      <CopyButton text="code" className="md-code-copy" idle="复制代码" />,
    );
    expect(html).toContain("复制代码");
    expect(html).toContain("md-code-copy");
  });
});

describe("Markdown 的代码块复制", () => {
  it("代码块有复制按钮，且语言标注可见", () => {
    const text = ["说明文字", "", "```js", "const a = 1;", "```"].join(NEWLINE);
    const html = renderToStaticMarkup(<Markdown text={text} />);

    expect(html).toContain("const a = 1;");
    expect(html).toContain("复制代码");
    expect(html).toContain(">js<");
  });

  it("两个代码块 => 两个复制按钮", () => {
    const text = ["```", "one", "```", "", "```", "two", "```"].join(NEWLINE);
    const html = renderToStaticMarkup(<Markdown text={text} />);
    expect(html.split("复制代码").length - 1).toBe(2);
  });

  it("没有代码块时不会凭空出现复制按钮", () => {
    const html = renderToStaticMarkup(<Markdown text="只有一段普通文字。" />);
    expect(html).not.toContain("复制代码");
    expect(html).toContain("只有一段普通文字。");
  });

  it("代码块里的空行也会被渲染出来（内容不能被裁掉）", () => {
    const text = ["```", "line1", "", "line3", "```"].join(NEWLINE);
    const html = renderToStaticMarkup(<Markdown text={text} />);
    expect(html).toContain("line1");
    expect(html).toContain("line3");
  });
});
