import { Fragment, useMemo, type ReactNode } from "react";

import { splitInline, splitMarkdownBlocks } from "../lib/markdown";

// 一个刻意做小的 Markdown 渲染器。
//
// 为什么不用 react-markdown + shiki：那两个包加起来比整个前端还大，而且高亮
// 需要按需加载才有意义。MVP 阶段真正高频的只有「代码块 + 行内代码 + 粗体」，
// 先把这三样做对；要换实现只需要替换这一个文件和 `lib/markdown.ts`。
//
// 解析逻辑在 `lib/markdown.ts`（纯函数、可单测），这里只负责把解析结果映射成
// React 元素。全程不碰 `dangerouslySetInnerHTML`：模型输出是不可信输入。

function renderInline(text: string): ReactNode[] {
  return splitInline(text).map((token, index) => {
    switch (token.kind) {
      case "code":
        return <code key={index}>{token.value}</code>;
      case "bold":
        return <strong key={index}>{token.value}</strong>;
      case "text":
        return <Fragment key={index}>{token.value}</Fragment>;
    }
  });
}

export const Markdown = ({ text }: { text: string }) => {
  const blocks = useMemo(() => splitMarkdownBlocks(text), [text]);

  return (
    <div className="md">
      {blocks.map((block, index) =>
        block.kind === "code" ? (
          <div className="md-code" key={index}>
            {block.language ? (
              <div className="md-code-lang">{block.language}</div>
            ) : null}
            <pre>
              <code>{block.content}</code>
            </pre>
          </div>
        ) : (
          <p key={index}>{renderInline(block.content)}</p>
        ),
      )}
    </div>
  );
};
