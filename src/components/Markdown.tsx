import { Fragment, useMemo, type ReactNode } from "react";

import { splitInline, splitMarkdownBlocks } from "../lib/markdown";
import { CopyButton } from "./CopyButton";

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
            {/* 头部固定占位：没有语言标注时也要有一行，
                否则「复制」按钮在代码块之间会上下跳动。 */}
            <div className="md-code-head">
              <span className="md-code-lang">{block.language ?? ""}</span>
              <CopyButton
                text={block.content}
                className="md-code-copy"
                idle="复制代码"
                title="复制这个代码块"
              />
            </div>
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
