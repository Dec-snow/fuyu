/**
 * 把异常变成一句人话。
 *
 * 为什么需要它：后端的错误是**结构化对象**（`{ code, message }`，见 Rust 的 `AppError`），
 * 而模板字符串会把它拼成 `[object Object]` —— 用户什么信息都得不到，
 * 只能看出「失败了」，看不出为什么。
 *
 * 这个问题（真机端到端测试发现的）在代码里出现过 **24 处**：
 * `pushNotice(\`导出失败：${e}\`)` 这种写法到处都是。所以修法不是逐个改成
 * `e.message`（那样还是会漏），而是统一走这一个函数。
 *
 * 取值顺序是有讲究的：
 * 1. 字符串直接用；
 * 2. `Error` 用 `message`；
 * 3. 对象先取 `message`（后端的正常形态）；
 * 4. 没有 message 就退回 `code`（至少让用户知道是哪一类失败）；
 * 5. 再不行才 `JSON.stringify`；
 * 6. 兜底 `String(...)`。
 *
 * 绝不返回空字符串：宁可能看到一个不好看的 JSON，也不要看到「失败了：」后面什么都没有。
 */
export function describeError(error: unknown): string {
  if (typeof error === "string") {
    return error.trim() || "未知错误";
  }
  if (error instanceof Error) {
    return error.message.trim() || error.name || "未知错误";
  }
  if (error && typeof error === "object") {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) {
      return message.trim();
    }
    const code = (error as { code?: unknown }).code;
    if (typeof code === "string" && code.trim()) {
      return code.trim();
    }
    try {
      const json = JSON.stringify(error);
      if (json && json !== "{}" && json !== "null") {
        return json;
      }
    } catch {
      // 循环引用之类：不值得为它崩掉错误提示本身
    }
  }
  const text = String(error);
  return text.trim() && text !== "[object Object]" ? text : "未知错误";
}
