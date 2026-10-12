import { useEffect, useMemo, useRef, useState } from "react";

import { visibleMessages } from "../lib/chat-state";
import {
  buildMessage,
  canAddAttachment,
  canAddDocument,
  decodeText,
  isOfficeDoc,
  looksBinary,
  MAX_ATTACHMENTS,
  type Attachment,
} from "../lib/attachments";
import { api } from "../services/api";
import { useChat } from "../stores/chat";
import { useUi } from "../stores/ui";
import { ModelPicker } from "./ModelPicker";
import { RolePicker } from "./RolePicker";

/** 一条消息最多几张图。与后端 `commands/history.rs` 的常量保持一致。 */
const MAX_IMAGES = 4;

/**
 * 允许的图片类型。必须和后端 `ALLOWED_IMAGE_PREFIXES` 一致。
 *
 * 前端以前只判断 `image/*`，于是 SVG / HEIC / BMP 都能通过预览，
 * 一直等到发送时才被后端拒掉 —— 那时整条消息（文字 + 图片）都已经没了。
 */
const ALLOWED_IMAGE_TYPES = [
  "image/png",
  "image/jpeg",
  "image/jpg",
  "image/webp",
  "image/gif",
];

/** 把 File 读成 data URL；失败返回 null（不抛，让调用方静默跳过这个文件）。 */
function fileToDataUrl(file: File): Promise<string | null> {
  return new Promise((resolve) => {
    const reader = new FileReader();
    reader.onload = () =>
      resolve(typeof reader.result === "string" ? reader.result : null);
    reader.onerror = () => resolve(null);
    reader.readAsDataURL(file);
  });
}

export function Composer() {
  const [text, setText] = useState("");
  /** 待发送的图片（data URL）。 */
  const [attachments, setAttachments] = useState<string[]>([]);
  /** 待发送的文本类文件（内容已经读成字符串）。 */
  const [files, setFiles] = useState<Attachment[]>([]);
  const [dragging, setDragging] = useState(false);
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const send = useChat((s) => s.send);
  const cancel = useChat((s) => s.cancel);
  const regenerate = useChat((s) => s.regenerate);
  const toolsEnabled = useChat((s) => s.toolsEnabled);
  const setToolsEnabled = useChat((s) => s.setToolsEnabled);
  const expanded = useUi((s) => s.expanded);
  const injectedInput = useUi((s) => s.injectedInput);

  // 与 MessageList 用同一套「可见消息」口径，否则「重新生成」按钮会在
  // 流式刚结束、数据库还没回读的那一小段时间里错误地消失。
  const stored = useChat((s) => s.messages);
  const live = useChat((s) => s.live);
  const routes = useChat((s) => s.routes);
  const activeId = useChat((s) => s.activeId);
  const messages = useMemo(
    () => visibleMessages({ messages: stored, live, activeId }),
    [stored, live, activeId],
  );

  // 「忙」要看的是**当前这个会话**在不在生成。
  //
  // 以前用的是全局 `streamingId`：在 A 里生成、切到 B，B 的输入框也显示
  // 「停止」、而且发不出消息；点一下「停止」反而把 A 的生成杀掉了。
  const busy = useMemo(
    () => Object.values(routes).some((route) => route.conversationId === activeId),
    [routes, activeId],
  );

  // 输入框高度跟着内容长，但不无上限地长。
  useEffect(() => {
    const el = areaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, expanded ? 180 : 96)}px`;
  }, [text, expanded, attachments.length]);

  // 悬浮窗被唤出时把焦点直接放到输入框 —— 「呼出即可打字」。
  useEffect(() => {
    areaRef.current?.focus();
  }, []);

  // 划词取词：把抓到的文本追加进输入框（已有草稿则换行追加）。
  useEffect(() => {
    if (injectedInput == null) return;
    setText((prev) => (prev.trim() ? `${prev}\n${injectedInput}` : injectedInput));
    useUi.getState().consumeInjectedInput();
    areaRef.current?.focus();
  }, [injectedInput]);

  /**
   * 收下粘贴 / 拖进来的图片。
   *
   * `FileReader` 读成 data URL 而不是走 Tauri 的文件 API：这样 WebView 里
   * 直接就能 `<img src>` 预览，也省掉一个需要授权的插件。
   */
  const addFiles = async (files: File[]) => {
    const images = files.filter((file) => file.type.startsWith("image/"));
    const docs = files.filter((file) => !file.type.startsWith("image/"));

    // 文本类文件走另一条路：要读字节、验是不是真的文本、还要卡大小。
    // 放在图片前面处理，这样「有图有文」的一次拖入两样都不丢。
    if (docs.length > 0) await addTextFiles(docs);

    if (images.length === 0) return;

    // 后端只收这几种位图。在这里就挡掉：否则用户看到预览一切正常，
    // 一直等到发送才被拒 —— 那时候整条消息（字 + 图）都已经丢了。
    const supported = images.filter((file) =>
      ALLOWED_IMAGE_TYPES.includes(file.type.toLowerCase()),
    );
    if (supported.length < images.length) {
      useUi
        .getState()
        .pushNotice("只支持 PNG / JPEG / WebP / GIF 图片，其它格式已忽略");
    }
    if (supported.length === 0) return;

    const room = Math.max(0, MAX_IMAGES - attachments.length);
    const accepted = supported.slice(0, room);

    // 并行读：4 张手机照片串行读会明显卡住输入框。
    const urls = (await Promise.all(accepted.map(fileToDataUrl))).filter(
      (url): url is string => url !== null,
    );
    if (urls.length > 0) {
      setAttachments((prev) => [...prev, ...urls].slice(0, MAX_IMAGES));
    }
    if (images.length > accepted.length) {
      useUi.getState().pushNotice(`一条消息最多 ${MAX_IMAGES} 张图片`);
    }
  };

  /**
   * 收下文本类文件（.txt / .md / 代码 / 配置…）。
   *
   * 逐个文件**独立**判断与拒绝，而不是「有一个不合格就整批放弃」：
   * 用户一次拖进来五个文件，其中有 .exe 也有 .md 是常事，
   * 全丢掉会让人以为「这个功能坏了」。
   *
   * 校验分三层，缺一不可（详见 `src/lib/attachments.ts`）：
   * 扩展名白名单 → 字节像不像二进制 → 是不是合法 UTF-8。
   */
  const addTextFiles = async (picked: File[]) => {
    const accepted: Attachment[] = [];
    for (const file of picked) {
      const doc = isOfficeDoc(file.name);

      // Office 文档走**后端解析**这条路：前端解 ZIP+XML 要引一堆库，
      // 而大小与 zip bomb 上限在后端才能强制执行。
      if (doc) {
        // 预检只看个数 / 类型 / 单文件大小；**合计**要等解析完按正文算
        // （详见 `canAddDocument` 的注释：用文件大小去比 512 KB 的合计上限，
        //  任何稍大的文档都会被提前拒掉）
        const pre = canAddDocument([...files, ...accepted], file.name, file.size);
        if (!pre.ok) {
          useUi.getState().pushNotice(`「${file.name}」已跳过：${pre.reason}`);
          continue;
        }
        const dataUrl = await fileToDataUrl(file);
        if (dataUrl === null) {
          useUi.getState().pushNotice(`「${file.name}」读取失败，已跳过`);
          continue;
        }
        try {
          const extracted = await api.extractDocument(file.name, dataUrl);
          const textBytes = new TextEncoder().encode(extracted.text).length;
          // 提取之后再用**正文大小**复核一次合计上限：文档本身 9 MB，
          // 但正文会被后端截到 256 KB，计入上下文的应该是正文。
          const post = canAddAttachment(
            [...files, ...accepted],
            file.name,
            textBytes,
            "document",
          );
          if (!post.ok) {
            useUi.getState().pushNotice(`「${file.name}」已跳过：${post.reason}`);
            continue;
          }
          accepted.push({
            name: file.name,
            text: extracted.text,
            bytes: textBytes,
            kind: "document",
            kindLabel: extracted.kind,
          });
          if (extracted.truncated) {
            useUi
              .getState()
              .pushNotice(`「${file.name}」内容过长，已截断到 ${extracted.chars} 字`);
          }
        } catch (e) {
          useUi.getState().pushNotice(`「${file.name}」解析失败：${e}`);
        }
        continue;
      }

      const check = canAddAttachment([...files, ...accepted], file.name, file.size);
      if (!check.ok) {
        useUi.getState().pushNotice(`「${file.name}」已跳过：${check.reason}`);
        continue;
      }

      const bytes = new Uint8Array(await file.arrayBuffer());
      if (looksBinary(bytes)) {
        // 扩展名可以改，字节不会骗人。
        useUi
          .getState()
          .pushNotice(`「${file.name}」看起来不是文本文件，已跳过`);
        continue;
      }
      const decoded = decodeText(bytes);
      if (decoded === null) {
        useUi
          .getState()
          .pushNotice(`「${file.name}」不是 UTF-8 文本，已跳过（可先转成 UTF-8）`);
        continue;
      }
      accepted.push({ name: file.name, text: decoded, bytes: bytes.length });
    }

    if (accepted.length > 0) setFiles((prev) => [...prev, ...accepted]);
  };

  const submit = async () => {
    if (busy) return;
    if (!text.trim() && attachments.length === 0 && files.length === 0) return;

    // 附件（文本文件）拼进正文：这样它天然进入历史、也会被持久化，
    // 不需要给数据库加字段。
    const value = buildMessage(text, files);
    const images = attachments;
    // **确认发出去了才清空草稿。**
    //
    // 以前是先清再发：后端一旦拒收（图片格式不对、还没配 API Key、
    // 历史超过 200 条…），用户辛苦打的字和贴的图一起消失，
    // 而提示还可能在别的地方 —— 他连「为什么没了」都不知道。
    const accepted = await send(value, images);
    if (accepted) {
      setText("");
      setAttachments([]);
      setFiles([]);
    }
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter 发送、Shift+Enter 换行；输入法组合中的 Enter 不能当发送。
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      void submit();
    }
  };

  const onPaste = (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(event.clipboardData.files);
    // 只有真的带图时才拦截，否则会破坏正常的文字粘贴。
    if (files.some((file) => file.type.startsWith("image/"))) {
      event.preventDefault();
      void addFiles(files);
    }
  };

  const lastIsAssistant = messages[messages.length - 1]?.role === "assistant";
  const canSend = text.trim().length > 0 || attachments.length > 0 || files.length > 0;

  return (
    <div
      className={`composer ${dragging ? "dragging" : ""}`}
      onDragOver={(e) => {
        // 不 preventDefault 的话浏览器会直接打开这个文件。
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragging(false);
        void addFiles(Array.from(e.dataTransfer.files));
      }}
    >
      <div className="composer-toolbar">
        <ModelPicker />
        <RolePicker />
        {files.length < MAX_ATTACHMENTS ? (
          <label className="ghost-button file-pick" title="附加文本 / 代码 / 配置文件，或 Word / Excel / PPT 文档（也可直接拖进来）">
            📄 文件
            <input
              type="file"
              multiple
              // 不设 `accept`：设了就选不到「被误命名的文本文件」，
              // 而那正是我们要用字节判断、并给出明确提示的情况。
              onChange={(e) => {
                const picked = Array.from(e.target.files ?? []);
                if (picked.length > 0) void addFiles(picked);
                // 清掉 value，否则再选同一个文件不会触发 change
                e.target.value = "";
              }}
            />
          </label>
        ) : null}        <button
          className={`tools-toggle ${toolsEnabled ? "on" : ""}`}
          title={
            toolsEnabled
              ? "已开启工具（时间 / 计算器 / 剪贴板 / 联网）"
              : "开启工具：让模型可以调用时间 / 计算器 / 剪贴板 / 联网"
          }
          onClick={() => setToolsEnabled(!toolsEnabled)}
        >
          🔧 {toolsEnabled ? "工具开" : "工具关"}
        </button>
        {!busy && lastIsAssistant && messages.length > 0 ? (
          <button className="ghost-button" onClick={() => void regenerate()}>
            ↻ 重新生成
          </button>
        ) : null}
      </div>

      {files.length > 0 ? (
        <div className="attachment-row">
          {files.map((file, index) => (
            <div className="file-chip" key={`${file.name}-${index}`}>
              <span className="file-chip-name" title={`${file.name}（${file.bytes} 字节）`}>
                📄 {file.name}
              </span>
              <span className="file-chip-size">
                {(file.bytes / 1024).toFixed(file.bytes < 10240 ? 1 : 0)} KB
              </span>
              <button
                className="attachment-remove"
                title="移除"
                onClick={() => setFiles((prev) => prev.filter((_, i) => i !== index))}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      ) : null}

      {attachments.length > 0 ? (
        <div className="attachment-row">
          {attachments.map((url, index) => (
            <div className="attachment" key={`${index}-${url.slice(-16)}`}>
              <img src={url} alt={`待发送图片 ${index + 1}`} />
              <button
                className="attachment-remove"
                title="移除"
                onClick={() =>
                  setAttachments((prev) => prev.filter((_, i) => i !== index))
                }
              >
                ×
              </button>
            </div>
          ))}
        </div>
      ) : null}

      <div className="composer-input">
        <textarea
          ref={areaRef}
          rows={1}
          value={text}
          placeholder={
            attachments.length > 0
              ? "给这张图配个问题…（可直接回车发送）"
              : files.length > 0
                ? "针对这些文件问点什么…（可直接回车发送）"
                : "问点什么…（Enter 发送，Shift+Enter 换行，可粘贴图片或拖入文件）"
          }
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
        />
        {busy ? (
          <button className="send-button stop" onClick={() => void cancel()}>
            停止
          </button>
        ) : (
          <button
            className="send-button"
            disabled={!canSend}
            onClick={() => void submit()}
          >
            发送
          </button>
        )}
      </div>

      {dragging ? <div className="drop-hint">松手即可添加图片或文本文件</div> : null}
    </div>
  );
}
