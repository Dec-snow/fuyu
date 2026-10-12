// 文本类文件附件的**校验与拼装**（纯逻辑，可单测）。
//
// 为什么把校验单独拎出来：这里挡的是三类「看起来很正常的输入」——
// - 把 .exe 改名成 .txt 拖进来（扩展名白名单挡不住，得看字节）；
// - 一个 20 MB 的日志文件（会把一次请求的上下文预算直接吃光）；
// - 文件内容里本身就有 ``` （会把我们的围栏提前闭合，后面全被当成正文）。
//
// 这三条都不会报错，只会让请求变大、变乱或者变成乱码。

/** 单个文件的大小上限。 */
export const MAX_ATTACHMENT_BYTES = 256 * 1024;
/** 一次最多几个附件。 */
export const MAX_ATTACHMENTS = 3;
/** 一次附件合计上限。 */
export const MAX_TOTAL_ATTACHMENT_BYTES = 512 * 1024;

/** 单个 Office 文档的大小上限（Rust 侧另有 12 MB 的 base64 上限兜着）。 */
export const MAX_DOCUMENT_BYTES = 9 * 1024 * 1024;

/**
 * 需要**后端解析**的 Office 文档扩展名。
 *
 * 这几个不能像文本文件那样在浏览器里直接读：docx / xlsx / pptx 是 ZIP + XML，
 * 前端要解就得引一堆库。所以它们走 `extract_document` 命令，由 Rust 提取出
 * 文字之后再当成普通附件用。
 */
export const OFFICE_EXTENSIONS: readonly string[] = ["docx", "xlsx", "pptx", "pdf"];

/** 是不是需要后端解析的 Office 文档。 */
export function isOfficeDoc(name: string): boolean {
  return OFFICE_EXTENSIONS.includes(extensionOf(name));
}

/**
 * 允许的扩展名（**白名单**）。
 *
 * 白名单而不是黑名单：黑名单永远列不全（`.dat`、`.bin`、`.db`…），
 * 而这份名单里的都是「人能读、模型读了有用」的纯文本。
 */
export const TEXT_EXTENSIONS: readonly string[] = [
  // 文档类
  "txt", "md", "markdown", "rst", "log", "csv", "tsv",
  // 数据 / 配置
  "json", "jsonl", "yaml", "yml", "toml", "ini", "conf", "env", "xml",
  // 代码
  "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt",
  "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "swift", "lua", "r",
  "sh", "bash", "zsh", "ps1", "bat", "cmd", "sql", "css", "scss", "less",
  "html", "htm", "vue", "svelte", "dart", "scala", "pl", "ex", "exs",
];

/** `notes.md` → `md`（没有扩展名返回空串，统一小写）。 */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return "";
  return name.slice(dot + 1).toLowerCase();
}

export function isAllowedExtension(name: string): boolean {
  return TEXT_EXTENSIONS.includes(extensionOf(name));
}

/**
 * 看起来像二进制吗？
 *
 * 扩展名可以被改，字节不会骗人：UTF-8 文本里几乎不会出现 NUL，
 * 控制字符的比例也很低。用两条**宽进严出**的判据（宁可放过也不误杀中文/emoji）：
 * - 出现 NUL 字节 → 二进制（UTF-8 文本不可能有 NUL）；
 * - 控制字符（除 \t \n \r）超过 10% → 二进制。
 */
export function looksBinary(bytes: Uint8Array): boolean {
  const limit = Math.min(bytes.length, 8192);
  if (limit === 0) return false;
  let control = 0;
  for (let i = 0; i < limit; i += 1) {
    const b = bytes[i];
    if (b === 0) return true;
    const isAllowedWhitespace = b === 9 || b === 10 || b === 13;
    if (b < 32 && !isAllowedWhitespace) control += 1;
  }
  return control / limit > 0.1;
}

/**
 * 按 UTF-8 解码；**不是合法 UTF-8 就返回 `null`**。
 *
 * 用 `fatal: true` 而不是默认的「替换成 U+FFFD」：默认行为会把一个 GBK 编码的
 * 文件变成满屏问号发给模型，而用户完全不知道发生了什么。宁可明确拒绝并告诉他
 * 「这个文件不是 UTF-8 文本」。
 */
export function decodeText(bytes: Uint8Array): string | null {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

export interface Attachment {
  name: string;
  text: string;
  /** 计入合计上限的字节数（文本文件是文件大小；文档是**提取出来的正文大小**）。 */
  bytes: number;
  /**
   * 来源类型：普通文本文件，还是从 Office 文档里提取出来的。
   *
   * 存在的意义是让附件块的标题说清「这是什么」—— 模型看到
   * `【附件（Word 文档）：报告.docx】` 就知道里面的表格是被拍平过的，
   * 而不是拿到一段来路不明的文本。
   */
  kind?: "text" | "document";
  /** Office 文档的人类可读类型名（「Word 文档」），由后端返回。 */
  kindLabel?: string;
}

/**
 * Office 文档的**预检**：只判个数、类型、单文件大小。
 *
 * 为什么不能直接用 [`canAddAttachment`]：那个还会算**合计**上限（512 KB），
 * 而文档在解析前的「字节数」是文件大小（可以到 9 MB）—— 拿它去比合计，
 * 任何稍大的文档都会被提前拒掉，功能等于废了。
 * 合计要等解析完、拿到**正文大小**之后再算（见 `Composer` 里的第二次检查）。
 */
export function canAddDocument(
  existing: Attachment[],
  name: string,
  fileBytes: number,
): { ok: true } | { ok: false; reason: string } {
  if (existing.length >= MAX_ATTACHMENTS) {
    return { ok: false, reason: `一次最多 ${MAX_ATTACHMENTS} 个文件` };
  }
  if (!isOfficeDoc(name)) {
    return { ok: false, reason: "只支持 .docx / .xlsx / .pptx 三种文档" };
  }
  if (fileBytes > MAX_DOCUMENT_BYTES) {
    return {
      ok: false,
      reason: `文档太大（${(fileBytes / 1048576).toFixed(1)} MB，上限 ${MAX_DOCUMENT_BYTES / 1048576} MB）`,
    };
  }
  return { ok: true };
}

/**
 * 能不能再加一个附件；返回拒绝的理由（用于给用户明确提示）。
 *
 * `kind` 决定按哪套大小上限判断：Office 文档本来就大（一张插图就几 MB），
 * 拿文本文件的 256 KB 去卡它会让这个功能完全没法用。
 */
export function canAddAttachment(
  existing: Attachment[],
  name: string,
  bytes: number,
  kind: "text" | "document" = "text",
): { ok: true } | { ok: false; reason: string } {
  if (kind === "document") {
    // 文档路径：个数 / 类型 / 单文件大小由 canAddDocument 判，
    // 这里只补**合计**（此时传入的应当是提取出来的正文大小）。
    const pre = canAddDocument(existing, name, bytes);
    if (!pre.ok) return pre;
  } else {
    if (existing.length >= MAX_ATTACHMENTS) {
      return { ok: false, reason: `一次最多 ${MAX_ATTACHMENTS} 个文件` };
    }
    if (!isAllowedExtension(name)) {
      const ext = extensionOf(name);
      return {
        ok: false,
        reason: ext
          ? `不支持 .${ext} 这类文件（只支持纯文本 / 代码 / 配置）`
          : "无法识别这个文件的类型（只支持纯文本 / 代码 / 配置）",
      };
    }
    if (bytes > MAX_ATTACHMENT_BYTES) {
      return {
        ok: false,
        reason: `文件太大（${(bytes / 1024).toFixed(0)} KB，上限 ${MAX_ATTACHMENT_BYTES / 1024} KB）`,
      };
    }
  }

  const total = existing.reduce((sum, a) => sum + a.bytes, 0) + bytes;
  if (total > MAX_TOTAL_ATTACHMENT_BYTES) {
    return {
      ok: false,
      reason: `附件合计太大（上限 ${MAX_TOTAL_ATTACHMENT_BYTES / 1024} KB）`,
    };
  }
  return { ok: true };
}

/**
 * 用**足够长的**围栏把文件内容包起来。
 *
 * 关键细节：文件内容里可能本来就有 ``` （比如它本身就是一篇 Markdown）。
 * 固定用三个反引号会被内容提前闭合，后面的内容就跑到围栏外面、被当成对话正文 ——
 * 于是「附加了一个文件」变成「模型收到了两段格式错乱的东西」。
 * 所以围栏长度取「内容里最长连续反引号 + 1」，且至少 3。
 */
export function fenceFor(text: string): string {
  let longest = 0;
  let current = 0;
  for (const ch of text) {
    if (ch === "`") {
      current += 1;
      longest = Math.max(longest, current);
    } else {
      current = 0;
    }
  }
  return "`".repeat(Math.max(3, longest + 1));
}

/** 一个附件展开成发给模型的文本块。 */
export function attachmentBlock(att: Attachment): string {
  const fence = fenceFor(att.text);
  // Office 文档不给语言标注（它不是代码），改在标题里说明来源类型
  const isDoc = att.kind === "document";
  const lang = isDoc ? "" : extensionOf(att.name);
  const title = isDoc
    ? `【附件（${att.kindLabel ?? "文档"}）：${att.name}】`
    : `【附件：${att.name}】`;
  return `${title}\n${fence}${lang}\n${att.text}\n${fence}`;
}

/**
 * 把输入框里的文字与附件拼成一条最终消息。
 *
 * 附件放在**前面**：先给材料、再给问题，模型的注意力顺序更自然；
 * 而且如果上下文预算不够、要从最旧的开始丢，先丢的是附件而不是用户的提问。
 */
export function buildMessage(text: string, attachments: Attachment[]): string {
  if (attachments.length === 0) return text;
  const blocks = attachments.map(attachmentBlock).join("\n\n");
  return text.trim().length > 0 ? `${blocks}\n\n${text}` : blocks;
}
