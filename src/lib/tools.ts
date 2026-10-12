// 工具名 → 给人看的标签。
//
// 前端**只用于展示**：要不要弹审批卡、能不能永久放行，一律由后端的
// `ToolDefinition.sensitive` / `exfiltrates` 决定。这里写错最多是文案不准，
// 不会造成权限漏洞。

const TOOL_LABELS: Record<string, string> = {
  get_current_time: "读取当前时间",
  calculator: "计算器",
  read_clipboard_text: "读取剪贴板",
  write_clipboard_text: "写入剪贴板",
  fetch_webpage: "抓取网页",
  web_search: "联网搜索",
  search_knowledge_base: "检索本地知识库",
  remember: "记入长期记忆",
};

/**
 * 内置工具名清单（技能的工具白名单用它做勾选项）。
 *
 * 与后端 `tools/registry.rs` 的清单**必须一致**：技能里写了一个不存在的工具名，
 * 后端会明确报错（而不是静默丢掉），所以这里漏一个的后果是「想在技能里勾它
 * 却勾不到」，一眼能看出来。
 */
export const KNOWN_TOOL_NAMES = [
  "get_current_time",
  "calculator",
  "read_clipboard_text",
  "write_clipboard_text",
  "fetch_webpage",
  "web_search",
  "search_knowledge_base",
  "remember",
] as const;

/** 把工具名翻成短标签；遇到不认识的工具名就原样返回（新工具也不会显示成空白）。 */
export function toolLabel(name: string): string {
  return TOOL_LABELS[name] ?? name;
}
