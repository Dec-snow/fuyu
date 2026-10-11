import { Fragment, useEffect, useMemo, useState, type ReactNode } from "react";

import { highlightCode, type HighlightNode } from "../lib/highlight";
import { isDisplayMath, looksSafe, renderMath } from "../lib/math";
import { isMermaidBlock, renderMermaid } from "../lib/mermaid";
import { splitInline, splitMarkdownBlocks } from "../lib/markdown";
import { CopyButton } from "./CopyButton";

// 一个刻意做小的 Markdown 渲染器。
//
// 为什么不用 react-markdown + shiki：那两个包加起来比整个前端还大。真正高频的
// 只有「代码块 + 行内代码 + 粗体 + 公式」，先把这几样做对；要换实现只需要替换
// 这一个文件和 `lib/markdown.ts`。
//
// 解析逻辑在 `lib/markdown.ts`（纯函数、可单测），这里只负责把解析结果映射成
// React 元素。
//
// ⚠️ 关于 `dangerouslySetInnerHTML`：这里**只有公式**那一处用到它，原因是
// KaTeX 没有节点树输出（详见 `lib/math.ts` 顶部那段说明：trust:false +
// looksSafe 兜底 + 失败退回源码）。**代码高亮仍然一次都不注入 HTML** ——
// 走的是 lowlight 的语法树（`lib/highlight.ts`），这是选它而不是
// `hljs.highlight()` 的直接原因。

/** 把高亮语法树映射成 React 元素（不生成任何 HTML 字符串）。 */
function renderHighlighted(nodes: HighlightNode[]): ReactNode[] {
  return nodes.map((node, index) =>
    node.type === "text" ? (
      <Fragment key={index}>{node.value}</Fragment>
    ) : (
      <span key={index} className={node.className.join(" ") || undefined}>
        {renderHighlighted(node.children)}
      </span>
    ),
  );
}

/**
 * 行内 / 块级公式。
 *
 * 三重降级：渲染失败 → 显示源码；兜底检查不过 → 显示源码；
 * 源码过长 → 显示源码。**任何情况下都不会出现一片空白** ——
 * 用户至少能看到 `$x^2$` 本身，从而知道这里原本有个公式。
 */
function Math({ tex, display }: { tex: string; display: boolean }) {
  const html = useMemo(() => renderMath(tex, display), [tex, display]);

  if (!html || !looksSafe(html)) {
    return <code className="md-math-fallback">{display ? `$$${tex}$$` : `$${tex}$`}</code>;
  }

  return (
    <span
      className={display ? "md-math md-math-block" : "md-math"}
      // 唯一的注入点，安全性由 `lib/math.ts` 的 trust:false + looksSafe 保证
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

function CodeBlock({ language, content }: { language: string; content: string }) {
  // 高亮结果是纯数据（节点树），不是 HTML 字符串 —— 见文件顶部的说明。
  const nodes = useMemo(() => highlightCode(content, language), [content, language]);

  return (
    <div className="md-code">
      {/* 头部固定占位：没有语言标注时也要有一行，
          否则「复制」按钮在代码块之间会上下跳动。 */}
      <div className="md-code-head">
        <span className="md-code-lang">{language}</span>
        <CopyButton
          text={content}
          className="md-code-copy"
          idle="复制代码"
          title="复制这个代码块"
        />
      </div>
      <pre>
        <code className={nodes ? "hljs" : undefined}>
          {nodes ? renderHighlighted(nodes) : content}
        </code>
      </pre>
    </div>
  );
}

/** 当前主题（mermaid 需要知道画深色图还是浅色图）。 */
function currentMermaidTheme(): "default" | "dark" {
  // SSR / 测试环境里没有 document
  if (typeof document === "undefined") return "default";
  return document.documentElement.dataset.theme === "dark" ? "dark" : "default";
}

/**
 * mermaid 图表。
 *
 * 三个刻意的设计：
 * - **异步渲染 + 加载态**：mermaid 是按需加载的（几百 KB），首次出现图表时
 *   会先闪一下「正在绘制图表…」，而不是让整条消息卡住不显示；
 * - **跟随主题**：`applyTheme` 改的是 `<html data-theme>`，React 不会因此重渲染，
 *   所以自己用 `MutationObserver` 盯这个属性 —— 否则切到深色后**新画的图仍是浅色**
 *   （mermaid 把主题缓存在初始化参数里，必须重新 initialize）；
 * - **降级可见**：画不出来就按 mermaid 源码块显示（同样带复制按钮），
 *   而不是留一块空白。
 */
function Mermaid({ code }: { code: string }) {
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [theme, setTheme] = useState(currentMermaidTheme);

  useEffect(() => {
    if (typeof MutationObserver === "undefined") return;
    const observer = new MutationObserver(() => setTheme(currentMermaidTheme()));
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let cancelled = false;
    setFailed(false);
    void renderMermaid(code, theme).then((result) => {
      if (cancelled) return;
      // 渲染失败时清掉旧图，避免主题切换后新旧两张图同时存在
      if (result) setSvg(result.svg);
      else {
        setSvg(null);
        setFailed(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [code, theme]);

  if (svg) {
    return (
      <div
        className="md-mermaid"
        // 第二个（也是最后一个）注入点，安全性见 lib/mermaid.ts 顶部说明
        dangerouslySetInnerHTML={{ __html: svg }}
      />
    );
  }
  if (failed) {
    return <CodeBlock language="mermaid" content={code} />;
  }
  return <div className="md-mermaid-loading">正在绘制图表…</div>;
}

function renderInline(text: string): ReactNode[] {
  return splitInline(text).map((token, index) => {
    switch (token.kind) {
      case "code":
        return <code key={index}>{token.value}</code>;
      case "bold":
        return <strong key={index}>{token.value}</strong>;
      case "math":
        return (
          <Math key={index} tex={token.value} display={isDisplayMath(token.value)} />
        );
      case "text":
        return <Fragment key={index}>{token.value}</Fragment>;
    }
  });
}

export const Markdown = ({ text }: { text: string }) => {
  const blocks = useMemo(() => splitMarkdownBlocks(text), [text]);

  return (
    <div className="md">
      {blocks.map((block, index) => {
        if (block.kind === "code") {
          // mermaid 代码块不走高亮，直接画图（画不出来时组件内部会退回源码）
          if (isMermaidBlock(block.language)) {
            return <Mermaid key={index} code={block.content} />;
          }
          return (
            <CodeBlock key={index} language={block.language} content={block.content} />
          );
        }
        if (block.kind === "math") {
          return <Math key={index} tex={block.content} display />;
        }
        return <p key={index}>{renderInline(block.content)}</p>;
      })}
    </div>
  );
};
