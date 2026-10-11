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
};

/** 把工具名翻成短标签；遇到不认识的工具名就原样返回（新工具也不会显示成空白）。 */
export function toolLabel(name: string): string {
  return TOOL_LABELS[name] ?? name;
}
