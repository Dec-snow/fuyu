import { useEffect, useRef, useState } from "react";

import {
  clampFontSize,
  MAX_FONT_SIZE,
  MIN_FONT_SIZE,
  MIN_WINDOW_OPACITY,
  opacityFromPercent,
  opacityToPercent,
} from "../lib/appearance";
import { sortModels } from "../lib/models";
import { blankRole } from "../lib/roles";
import { formatDiagnostics, hasProblems } from "../lib/diagnostics";
import {
  describeDocument,
  describeHit,
  describeMemory,
  describeMemoryStats,
  describeStats,
  kbFileKind,
  previewText,
  titleFromFileName,
} from "../lib/kb";
import { toolLabel } from "../lib/tools";
import {
  barWidth,
  formatCost,
  formatTokens,
  maxDayTokens,
  shortDay,
  totalTokens,
} from "../lib/usage";
import { api } from "../services/api";
import { useChat } from "../stores/chat";
import { applyAppearance, previewAppearance, useConfig } from "../stores/config";
import { useProviders } from "../stores/providers";
import { useUi } from "../stores/ui";
import type {
  Diagnostics,
  KbDocument,
  KbHit,
  KbStats,
  Memory,
  MemoryStats,
  RolePreset,
  UsageReport,
} from "../types";
import { CopyButton } from "./CopyButton";

type Tab = "models" | "appearance" | "general" | "kb" | "data" | "about";

const TABS: { id: Tab; label: string }[] = [
  { id: "models", label: "模型" },
  { id: "appearance", label: "对话" },
  { id: "general", label: "通用" },
  { id: "kb", label: "知识库" },
  { id: "data", label: "数据" },
  { id: "about", label: "关于" },
];

export function SettingsPanel() {
  const [tab, setTab] = useState<Tab>("models");
  const close = useUi((s) => s.closeSettings);

  return (
    <div className="settings-backdrop" onClick={close}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div className="settings-head">
          <div className="settings-tabs">
            {TABS.map((t) => (
              <button
                key={t.id}
                className={t.id === tab ? "active" : ""}
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </div>
          <button className="icon-button" onClick={close}>
            ✕
          </button>
        </div>

        <div className="settings-body">
          {tab === "models" ? <ModelsTab /> : null}
          {tab === "appearance" ? <AppearanceTab /> : null}
          {tab === "general" ? <GeneralTab /> : null}
          {tab === "kb" ? <KnowledgeTab /> : null}
          {tab === "data" ? <DataTab /> : null}
          {tab === "about" ? <AboutTab /> : null}
        </div>
      </div>
    </div>
  );
}

function ModelsTab() {
  const state = useProviders((s) => s.state);
  const store = useProviders();
  const [keyDraft, setKeyDraft] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<Record<string, string>>({});

  const note = (id: string, text: string) =>
    setMessage((m) => ({ ...m, [id]: text }));

  if (!state) return <div className="settings-hint">载入中…</div>;

  return (
    <div className="settings-section">
      <p className="settings-note">
        API Key 保存在 Windows 凭据管理器里，<b>不写入配置文件</b>，也不会回传给界面 ——
        这里只显示「是否已配置」。
      </p>

      {state.providers.map((provider) => (
        <div className="provider-card" key={provider.id}>
          <div className="provider-head">
            <span className="provider-name">{provider.name}</span>
            <span className={`key-badge ${provider.hasKey ? "ok" : "missing"}`}>
              {provider.hasKey ? "已配置 Key" : "未配置 Key"}
            </span>
            {/* 预设不允许删（后端也会拒），所以按钮只给自定义服务商。
                以前这个入口完全没有：`delete_provider` 和 `useProviders.remove`
                都写好了，却没有任何组件调用它们。 */}
            {provider.isPreset ? null : (
              <button
                className="provider-delete"
                title="删除这个服务商（连同它的 API Key）"
                disabled={busy === provider.id}
                onClick={async () => {
                  if (
                    !window.confirm(
                      `删除服务商「${provider.name}」？它的 API Key 也会一并从凭据管理器里清除。`,
                    )
                  ) {
                    return;
                  }
                  setBusy(provider.id);
                  try {
                    await store.remove(provider.id);
                  } catch (e) {
                    note(provider.id, `删除失败：${String(e)}`);
                  } finally {
                    setBusy(null);
                  }
                }}
              >
                删除
              </button>
            )}
          </div>

          <input
            className="text-input"
            value={keyDraft[provider.id] ?? ""}
            placeholder={provider.hasKey ? "已保存，留空则不改动" : "粘贴 API Key"}
            type="password"
            onChange={(e) =>
              setKeyDraft((d) => ({ ...d, [provider.id]: e.target.value }))
            }
          />

          <BaseUrlEditor providerId={provider.id} initial={provider.baseUrl} />

          <div className="provider-actions">
            <button
              disabled={busy === provider.id}
              onClick={async () => {
                const value = (keyDraft[provider.id] ?? "").trim();
                if (!value) {
                  note(provider.id, "请先填入 Key");
                  return;
                }
                setBusy(provider.id);
                try {
                  await store.setApiKey(provider.id, value);
                  setKeyDraft((d) => ({ ...d, [provider.id]: "" }));
                  note(provider.id, "已保存到系统凭据管理器");
                } catch (e) {
                  note(provider.id, String(e));
                } finally {
                  setBusy(null);
                }
              }}
            >
              保存 Key
            </button>

            <button
              disabled={busy === provider.id}
              onClick={async () => {
                setBusy(provider.id);
                note(provider.id, "拉取中…");
                try {
                  const keys = await store.fetchModels(provider.id);
                  if (keys.length === 0) {
                    note(provider.id, "该服务商没有返回模型列表");
                  } else {
                    await store.addModels(provider.id, keys);
                    note(provider.id, `已加入 ${keys.length} 个模型`);
                  }
                } catch (e) {
                  note(
                    provider.id,
                    `拉取失败（很多厂商不实现 /models，可手动添加）：${String(e)}`,
                  );
                } finally {
                  setBusy(null);
                }
              }}
            >
              拉取模型列表
            </button>

            <button
              disabled={busy === provider.id}
              onClick={async () => {
                const model = provider.models[0]?.key;
                if (!model) {
                  note(provider.id, "先添加一个模型");
                  return;
                }
                setBusy(provider.id);
                note(provider.id, "测试中…");
                try {
                  note(provider.id, await api.testProvider(provider.id, model));
                } catch (e) {
                  note(provider.id, `测试失败：${String(e)}`);
                } finally {
                  setBusy(null);
                }
              }}
            >
              测试连接
            </button>
          </div>

          <AddModelRow providerId={provider.id} />

          <div className="model-tags">
            {provider.models.length === 0 ? (
              <span className="settings-hint">还没有模型</span>
            ) : null}
            {sortModels(provider.models).map((model) => (
              <span
                key={model.key}
                className={`model-tag ${
                  state.defaultProviderId === provider.id &&
                  state.defaultModelKey === model.key
                    ? "active"
                    : ""
                }`}
              >
                <button
                  className={`model-tag-star${model.favorite ? " on" : ""}`}
                  title={
                    model.favorite
                      ? "取消收藏"
                      : "收藏（在输入框上方的下拉里排到前面）"
                  }
                  aria-pressed={model.favorite}
                  onClick={() =>
                    void store.setFavorite(
                      provider.id,
                      model.key,
                      !model.favorite,
                    )
                  }
                >
                  {model.favorite ? "★" : "☆"}
                </button>
                <button
                  className="model-tag-main"
                  title="设为默认"
                  onClick={() => void store.setDefault(provider.id, model.key)}
                >
                  {model.key}
                </button>
                <button
                  className="model-tag-remove"
                  title="移除"
                  onClick={() => void store.removeModel(provider.id, model.key)}
                >
                  ×
                </button>
              </span>
            ))}
          </div>

          {message[provider.id] ? (
            <div className="provider-message">{message[provider.id]}</div>
          ) : null}
        </div>
      ))}

      <AddProviderForm />
    </div>
  );
}

/**
 * 添加一个自定义服务商。
 *
 * 在这之前，用户只能用内置的 9 个预设：`upsert_provider` 早就支持「带一个
 * 新 id 调进来就是新建」，但界面上没有任何入口 —— 想接一个自建 / 中转
 * 服务（New API、One API、公司内网网关之类）就完全没办法。
 */
function AddProviderForm() {
  const upsert = useProviders((s) => s.upsert);
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [message, setMessage] = useState("");
  const [saving, setSaving] = useState(false);

  const submit = async () => {
    const trimmedName = name.trim();
    const trimmedUrl = baseUrl.trim();
    if (!trimmedName || !trimmedUrl) {
      setMessage("名称和 Base URL 都要填");
      return;
    }

    setSaving(true);
    setMessage("");
    try {
      await upsert({
        // id 只用于内部索引和凭据存储的键，不需要可读；
        // 加随机后缀是为了避免同一毫秒内连点两次撞成同一个 id。
        id: `custom-${Date.now().toString(36)}-${Math.random()
          .toString(36)
          .slice(2, 6)}`,
        name: trimmedName,
        baseUrl: trimmedUrl,
        models: model.trim() ? [{ key: model.trim(), favorite: false }] : [],
        isPreset: false,
      });
      setName("");
      setBaseUrl("");
      setModel("");
      setMessage("已添加。别忘了给它填一个 API Key。");
    } catch (e) {
      setMessage(`添加失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  if (!open) {
    return (
      <button className="ghost-button" onClick={() => setOpen(true)}>
        + 添加自定义服务商
      </button>
    );
  }

  return (
    <div className="provider-card">
      <div className="provider-head">
        <span className="provider-name">添加自定义服务商</span>
      </div>

      <input
        className="text-input"
        value={name}
        placeholder="名称，例如 公司网关"
        onChange={(e) => setName(e.target.value)}
      />
      <input
        className="text-input"
        value={baseUrl}
        placeholder="Base URL，例如 https://gateway.example.com/v1"
        onChange={(e) => setBaseUrl(e.target.value)}
      />
      <input
        className="text-input"
        value={model}
        placeholder="模型名（可留空，之后再添加）"
        onChange={(e) => setModel(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void submit();
        }}
      />

      <div className="provider-actions">
        <button disabled={saving} onClick={() => void submit()}>
          {saving ? "添加中…" : "添加"}
        </button>
        <button
          className="ghost-button"
          onClick={() => {
            setOpen(false);
            setMessage("");
          }}
        >
          取消
        </button>
      </div>

      {message ? <div className="provider-message">{message}</div> : null}
    </div>
  );
}

function BaseUrlEditor({
  providerId,
  initial,
}: {
  providerId: string;
  initial: string;
}) {
  const upsert = useProviders((s) => s.upsert);
  const state = useProviders((s) => s.state);
  const [value, setValue] = useState(initial);
  const [message, setMessage] = useState("");
  const [saving, setSaving] = useState(false);

  useEffect(() => setValue(initial), [initial]);

  const provider = state?.providers.find((p) => p.id === providerId);

  const save = async () => {
    if (!provider || saving) return;
    setSaving(true);
    setMessage("");
    try {
      await upsert({
        id: provider.id,
        name: provider.name,
        baseUrl: value.trim(),
        models: [],
        isPreset: provider.isPreset,
      });
      setMessage("已保存");
    } catch (e) {
      // 以前这里是 `void upsert(...)`：后端拒绝（比如忘了写 http://）
      // 时什么都不显示 —— 输入框留着错的内容、配置也没变，
      // 看起来就是「这个按钮坏了」。
      setMessage(`保存失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div className="base-url-row">
        <input
          className="text-input"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
          placeholder="Base URL，例如 https://api.deepseek.com/v1"
        />
        <button onClick={() => void save()} disabled={saving}>
          {saving ? "保存中…" : "保存"}
        </button>
      </div>
      {message ? <div className="provider-message">{message}</div> : null}
    </>
  );
}

function AddModelRow({ providerId }: { providerId: string }) {
  const addModels = useProviders((s) => s.addModels);
  const [value, setValue] = useState("");

  return (
    <div className="base-url-row">
      <input
        className="text-input"
        value={value}
        placeholder="手动添加模型名，例如 deepseek-chat"
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && value.trim()) {
            void addModels(providerId, [value.trim()]);
            setValue("");
          }
        }}
      />
      <button
        onClick={() => {
          if (!value.trim()) return;
          void addModels(providerId, [value.trim()]);
          setValue("");
        }}
      >
        添加
      </button>
    </div>
  );
}

/**
 * 卸载时补交「改了但还没落盘」的草稿。
 *
 * 设置面板里好几个字段都是「先改草稿，过一会儿或失焦才提交」。问题是：
 * 切到别的标签页、按 Esc 关掉面板都会**卸载**组件，而卸载既不会触发防抖
 * 定时器、也不会触发 `blur`（移除一个正在聚焦的元素不产生 blur 事件）——
 * 用户刚敲的一整段系统提示词就这样静默消失了。
 *
 * 用 ref 读最新值、依赖数组留空，保证只在真正卸载时补交一次；如果直接依赖
 * 回调（父组件每次渲染都会新建闭包），每次渲染都会误触发一次提交。
 */
function useFlushOnUnmount(dirty: boolean, flush: () => void) {
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const flushRef = useRef(flush);
  flushRef.current = flush;

  useEffect(
    () => () => {
      if (dirtyRef.current) flushRef.current();
    },
    [],
  );
}

/**
 * 带本地草稿的数字输入。
 *
 * 为什么不能直接 `onChange` 就落盘：`update_settings` 每次都会把整份配置
 * 原子写一次（临时文件 + `fsync` + rename）。逐字符提交的话，输入
 * 「32000」这一个动作就是 5 次带 fsync 的磁盘写 —— 卡顿而且毫无意义。
 * 这里改成「打字只改草稿，失焦、回车或卸载才提交」。
 */
function NumberField({
  value,
  min,
  max,
  step,
  onCommit,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onCommit: (next: number) => void;
}) {
  const [draft, setDraft] = useState(String(value));

  useEffect(() => setDraft(String(value)), [value]);

  const commit = () => {
    const parsed = Number(draft);
    if (!Number.isFinite(parsed) || draft.trim() === "") {
      setDraft(String(value));
      return;
    }
    const clamped = Math.min(max, Math.max(min, Math.round(parsed)));
    setDraft(String(clamped));
    if (clamped !== value) onCommit(clamped);
  };

  // Esc 关面板不会触发 blur，靠这个补交。
  useFlushOnUnmount(draft !== String(value), commit);

  return (
    <input
      type="number"
      value={draft}
      min={min}
      max={max}
      step={step}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
      }}
    />
  );
}

/**
 * 系统提示词文本框：同样先改草稿，停手 500ms 再落盘。
 *
 * 提示词往往写得很长，逐字符落盘意味着敲一篇 500 字的提示词就是 500 次
 * 临时文件 + fsync + rename。
 */
function DebouncedTextarea({
  value,
  rows,
  onCommit,
}: {
  value: string;
  rows: number;
  onCommit: (next: string) => void;
}) {
  const [draft, setDraft] = useState(value);

  useEffect(() => setDraft(value), [value]);

  useEffect(() => {
    if (draft === value) return;
    const timer = window.setTimeout(() => onCommit(draft), 500);
    return () => window.clearTimeout(timer);
  }, [draft, value, onCommit]);

  // 停手不到 500ms 就切标签 / 关面板：把还没落盘的编辑补交上去。
  useFlushOnUnmount(draft !== value, () => onCommit(draft));

  return (
    <textarea
      rows={rows}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
    />
  );
}

/**
 * 通用滑块：拖动时只看草稿，停手 350ms 才落盘。
 *
 * 否则拖一次滑块会产生几十次 fsync —— 每次都要写临时文件、刷盘、改名字。
 * （temperature、窗口不透明度、字号共用这一段。）
 *
 * `onPreview` 是给「改完要立刻看见」的项用的：拖动时先改 CSS 变量，
 * 落盘仍然走 350ms 节流。
 */
function RangeField({
  value,
  min,
  max,
  step,
  onCommit,
  onPreview,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  onCommit: (next: number) => void;
  onPreview?: (next: number) => void;
}) {
  const [draft, setDraft] = useState(value);

  useEffect(() => setDraft(value), [value]);

  useEffect(() => {
    if (draft === value) return;
    const timer = window.setTimeout(() => onCommit(draft), 350);
    return () => window.clearTimeout(timer);
    // onPreview 不参与依赖：它是即时生效的，不该触发提交计时器。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft, value, onCommit]);

  // 拖完 350ms 内就关面板的话，把最后一次的值补交上去。
  useFlushOnUnmount(draft !== value, () => onCommit(draft));

  return (
    <input
      type="range"
      min={min}
      max={max}
      step={step}
      value={draft}
      onChange={(e) => {
        const next = Number(e.target.value);
        setDraft(next);
        onPreview?.(next);
      }}
    />
  );
}

function TemperatureField({
  value,
  onCommit,
}: {
  value: number;
  onCommit: (next: number) => void;
}) {
  return (
    <RangeField value={value} min={0} max={2} step={0.1} onCommit={onCommit} />
  );
}

/**
 * 角色（人设）编辑器。
 *
 * 三条规则写在界面上而不是藏在代码里：
 * - 内置模板**可以改、不能删**（删了就找不回来，用户只能手工补）；
 * - 名字或提示词为空时**不提交**，本地回退并给一句提示 ——
 *   后端会拒绝空值，与其弹一个「保存失败」的红条，不如根本不发出去；
 * - 改完立刻生效于**下一个问题**（解析发生在发送那一刻，不是建会话时），
 *   已有会话不会因为改了措辞而需要重建。
 */
function RoleEditor() {
  const config = useConfig((s) => s.config);
  const patch = useConfig((s) => s.patch);
  const pushNotice = useUi((s) => s.pushNotice);
  if (!config) return null;

  const roles = config.roles ?? [];

  // 失败时 store 里已经写了 error（会渲染成提示条），这里只负责别让
  // 未处理的 promise 拒绝冒到控制台。
  const save = (next: RolePreset[]) => {
    void patch({ roles: next }).catch(() => undefined);
  };

  return (
    <div className="field">
      <span>角色（人设）—— 在输入框上方按会话切换；内置模板可改不可删</span>
      <div className="role-editor">
        {roles.map((role) => (
          <div className="role-row" key={role.id}>
            <div className="role-row-head">
              <input
                className="role-name"
                value={role.name}
                onChange={(e) =>
                  save(
                    roles.map((r) =>
                      r.id === role.id ? { ...r, name: e.target.value } : r,
                    ),
                  )
                }
                onBlur={() => {
                  if (role.name.trim() === "") {
                    pushNotice("角色名字不能为空");
                    // 名字被清空时不让它停留在空值上：后端会拒，界面也会显示一个
                    // 「没有名字的角色」——回退成默认名比报错更省事。
                    save(
                      roles.map((r) =>
                        r.id === role.id ? { ...r, name: "未命名角色" } : r,
                      ),
                    );
                  }
                }}
              />
              {role.builtin ? (
                <span className="role-tag">内置</span>
              ) : (
                <button
                  className="role-delete"
                  title="删除这个角色"
                  onClick={() => {
                    if (window.confirm(`删除角色「${role.name}」？`)) {
                      save(roles.filter((r) => r.id !== role.id));
                    }
                  }}
                >
                  删除
                </button>
              )}
            </div>
            <DebouncedTextarea
              value={role.prompt}
              rows={3}
              onCommit={(next) => {
                if (next.trim() === "") {
                  pushNotice(`角色「${role.name}」的提示词不能为空`);
                  return;
                }
                save(
                  roles.map((r) => (r.id === role.id ? { ...r, prompt: next } : r)),
                );
              }}
            />
          </div>
        ))}

        <button
          className="ghost-button"
          onClick={() => save([...roles, blankRole()])}
        >
          + 新增角色
        </button>
      </div>
    </div>
  );
}

function AppearanceTab() {
  const config = useConfig((s) => s.config);
  const patch = useConfig((s) => s.patch);
  if (!config) return <div className="settings-hint">载入中…</div>;

  /**
   * 外观类设置提交失败时，把 CSS 变量退回**配置里的值**。
   *
   * 不退回的话界面会停在一个「看起来生效了、其实没保存」的状态 ——
   * 下次启动又变回去，用户会认为设置不生效。
   */
  const commit = (next: Parameters<typeof patch>[0]) => {
    void patch(next).catch(() => {
      const current = useConfig.getState().config;
      if (current) applyAppearance(current);
    });
  };

  return (
    <div className="settings-section">
      <label className="field">
        <span>主题</span>
        <select
          value={config.theme}
          onChange={(e) =>
            void patch({ theme: e.target.value as "system" | "light" | "dark" })
          }
        >
          <option value="system">跟随系统</option>
          <option value="light">浅色</option>
          <option value="dark">深色</option>
        </select>
      </label>

      <label className="field">
        <span>
          窗口不透明度（{opacityToPercent(config.windowOpacity)}%）——
          只影响背景，文字始终清晰
        </span>
        <RangeField
          value={opacityToPercent(config.windowOpacity)}
          min={Math.round(MIN_WINDOW_OPACITY * 100)}
          max={100}
          step={1}
          onPreview={(percent) =>
            previewAppearance({ windowOpacity: opacityFromPercent(percent) })
          }
          onCommit={(percent) =>
            commit({ windowOpacity: opacityFromPercent(percent) })
          }
        />
      </label>

      <label className="field">
        <span>
          正文字号（{clampFontSize(config.messageFontSize)}px）——
          影响聊天内容与输入框
        </span>
        <RangeField
          value={clampFontSize(config.messageFontSize)}
          min={MIN_FONT_SIZE}
          max={MAX_FONT_SIZE}
          step={0.5}
          onPreview={(px) => previewAppearance({ messageFontSize: px })}
          onCommit={(px) => commit({ messageFontSize: px })}
        />
      </label>

      <label className="field">
        <span>系统提示词</span>
        <DebouncedTextarea
          value={config.systemPrompt}
          rows={4}
          onCommit={(next) => void patch({ systemPrompt: next })}
        />
      </label>

      <RoleEditor />

      <label className="field">
        <span>temperature（{config.temperature.toFixed(1)}）—— 越高回答越发散</span>
        <TemperatureField
          value={config.temperature}
          onCommit={(next) => void patch({ temperature: next })}
        />
      </label>

      <label className="field">
        <span>max_tokens（0 表示不限制）</span>
        <NumberField
          value={config.maxTokens ?? 0}
          min={0}
          max={1_000_000}
          step={256}
          onCommit={(next) => void patch({ maxTokens: next })}
        />
      </label>

      <label className="field">
        <span>
          上下文预算（token）—— 超出时从最旧的消息开始省略，并在回答上标注
        </span>
        <NumberField
          value={config.contextBudgetTokens}
          min={1_000}
          max={1_000_000}
          step={1_000}
          onCommit={(next) => void patch({ contextBudgetTokens: next })}
        />
      </label>
    </div>
  );
}

function GeneralTab() {
  const config = useConfig((s) => s.config);
  const patch = useConfig((s) => s.patch);
  const setShortcut = useConfig((s) => s.setShortcut);
  const setQuickActionShortcut = useConfig((s) => s.setQuickActionShortcut);
  const [shortcut, setShortcutDraft] = useState("");
  const [quickShortcut, setQuickShortcutDraft] = useState("");
  const [searchEndpoint, setSearchDraft] = useState("");
  const [notice, setNotice] = useState("");

  // 只在**对应的那个字段**变了时才重置草稿。
  //
  // 以前依赖的是整个 `config` 对象，而每次 `patch` 都会换一个新对象
  // （`set({ config })`）—— 于是「粘好 SearXNG 地址还没点应用、顺手勾一下
  // 记住窗口位置」就会把刚粘的地址冲回空值。
  useEffect(() => {
    if (!config) return;
    setShortcutDraft(config.shortcut);
  }, [config?.shortcut]);

  useEffect(() => {
    if (!config) return;
    setQuickShortcutDraft(config.quickActionShortcut);
  }, [config?.quickActionShortcut]);

  useEffect(() => {
    if (!config) return;
    setSearchDraft(config.webSearchEndpoint ?? "");
  }, [config?.webSearchEndpoint]);

  if (!config) return <div className="settings-hint">载入中…</div>;

  const grantedTools = config.alwaysAllowedTools ?? [];

  /** `name` 传 `null` 表示撤销全部。 */
  const revokeTool = async (name: string | null) => {
    const next = name === null ? [] : grantedTools.filter((t) => t !== name);
    try {
      await patch({ alwaysAllowedTools: next });
      setNotice(
        name === null ? "已撤销全部永久授权" : `已撤销「${toolLabel(name)}」的永久授权`,
      );
    } catch (e) {
      setNotice(`撤销失败：${e instanceof Error ? e.message : String(e)}`);
    }
  };

  return (
    <div className="settings-section">
      <label className="field">
        <span>唤出快捷键</span>
        <div className="base-url-row">
          <input
            className="text-input"
            value={shortcut}
            onChange={(e) => setShortcutDraft(e.target.value)}
          />
          <button
            onClick={async () => {
              try {
                await setShortcut(shortcut.trim());
                setNotice("已生效");
              } catch (e) {
                setNotice(String(e));
              }
            }}
          >
            应用
          </button>
        </div>
      </label>
      {notice ? <div className="provider-message">{notice}</div> : null}

      <label className="field">
        <span>划词快捷键（默认 Alt+Q，被占用时可在这里改）</span>
        <div className="base-url-row">
          <input
            className="text-input"
            value={quickShortcut}
            onChange={(e) => setQuickShortcutDraft(e.target.value)}
          />
          <button
            onClick={async () => {
              try {
                await setQuickActionShortcut(quickShortcut.trim());
                setNotice("划词快捷键已生效");
              } catch (e) {
                setNotice(String(e));
              }
            }}
          >
            应用
          </button>
        </div>
      </label>

      <AutostartField />

      <label className="field checkbox">
        <input
          type="checkbox"
          checked={config.hideOnBlur}
          onChange={(e) => void patch({ hideOnBlur: e.target.checked })}
        />
        <span>失去焦点时自动隐藏</span>
      </label>

      <label className="field checkbox">
        <input
          type="checkbox"
          checked={config.rememberWindowPosition}
          onChange={(e) => void patch({ rememberWindowPosition: e.target.checked })}
        />
        <span>记住窗口位置</span>
      </label>

      <label className="field checkbox">
        <input
          type="checkbox"
          checked={config.quickActionEnabled}
          onChange={async (e) => {
            // 先落盘再重新注册：`reapply_shortcuts` 是按配置注册的，
            // 反过来调用会把新的开关状态漏掉。
            await patch({ quickActionEnabled: e.target.checked });
            await api.reapplyShortcuts();
          }}
        />
        <span>启用划词助手（在其他程序里选中文字后按 Alt+Q 取词）</span>
      </label>

      <label className="field">
        <span>
          联网搜索端点（SearXNG，需开启 format=json；留空 = 关闭联网搜索）
        </span>
        <div className="base-url-row">
          <input
            className="text-input"
            placeholder="https://searx.example"
            value={searchEndpoint}
            onChange={(e) => setSearchDraft(e.target.value)}
          />
          <button
            onClick={async () => {
              try {
                await patch({ webSearchEndpoint: searchEndpoint.trim() });
                setNotice(
                  searchEndpoint.trim()
                    ? "已保存，联网搜索工具会在下次提问时可用"
                    : "已关闭联网搜索",
                );
              } catch (e) {
                setNotice(String(e));
              }
            }}
          >
            应用
          </button>
        </div>
      </label>

      <label className="field">
        <span>HTTP 代理（留空表示不使用）</span>
        <input
          className="text-input"
          value={config.proxyUrl ?? ""}
          placeholder="http://127.0.0.1:7890 或 socks5://127.0.0.1:1080"
          onChange={(e) => void patch({ proxyUrl: e.target.value })}
        />
      </label>

      <label className="field checkbox">
        <input
          type="checkbox"
          checked={config.useSystemProxy}
          onChange={(e) => void patch({ useSystemProxy: e.target.checked })}
        />
        <span>
          使用系统代理
          <em className="field-hint">
            读取系统与环境变量（`HTTP_PROXY` / `HTTPS_PROXY`）里的代理设置；
            上面填了地址时以填的为准
          </em>
        </span>
      </label>

      {/* 「永久允许」的工具只能在这里撤销。
          授权是在审批卡上给出的（写进 providers.json），但之前**没有任何界面**
          能查看或收回它 —— 撤销的唯一办法是手工编辑配置文件。 */}
      <div className="field">
        <span>
          已永久允许的工具
          <em className="field-hint">
            这些工具以后不会再弹审批卡；数据外发型工具本来就不允许永久放行
          </em>
        </span>
        {grantedTools.length === 0 ? (
          <div className="settings-hint">还没有永久允许过任何工具</div>
        ) : (
          <div className="granted-tools">
            {grantedTools.map((name) => (
              <button
                key={name}
                className="granted-chip"
                title="撤销「永久允许」"
                onClick={() => void revokeTool(name)}
              >
                {toolLabel(name)} ×
              </button>
            ))}
            <button
              className="ghost-button"
              onClick={() => void revokeTool(null)}
            >
              全部撤销
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** 把字节数写成人能读的形式。 */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1048576).toFixed(2)} MB`;
}

/**
 * 数据页：备份（导出 / 导入）与危险操作。
 *
 * 导出写成文件、导入选文件，都**不引入额外插件**：
 * - 导入用 `<input type=file>`（WebView 原生支持，能可靠拿到文件内容）；
 * - 导出由后端写到 `<应用数据目录>/exports/`，把完整路径显示出来并允许一键复制。
 *
 * 为什么不做「另存为」对话框：那需要 `tauri-plugin-dialog` 与相应的权限声明。
 * 先把「备份真的能存下来」做扎实，对话框是下一步。
 */
/**
 * 用量与费用估算。
 *
 * 两条「宁可说不知道」的规则，都体现在界面上：
 * - 价格表里认不出来的模型**不猜价**，而是列出来并标明「未计入」——
 *   悄悄按 0 算会给出一个看起来很确定的错数字，那比不给数字更糟；
 * - 费用一律标注「估算，仅供参考」：价格表是内置的、会过时。
 */
function UsagePanel() {
  const [report, setReport] = useState<UsageReport | null>(null);
  const [days, setDays] = useState(14);
  const pushNotice = useUi((s) => s.pushNotice);

  useEffect(() => {
    void api
      .usageReport(days)
      .then(setReport)
      .catch((e) => pushNotice(`读取用量失败：${e}`));
  }, [days, pushNotice]);

  const max = report ? maxDayTokens(report.days) : 0;

  return (
    <div className="field">
      <span>用量与费用</span>
      <div className="danger-row">
        <select
          value={days}
          onChange={(e) => setDays(Number(e.target.value))}
          className="usage-range"
        >
          <option value={7}>最近 7 天</option>
          <option value={14}>最近 14 天</option>
          <option value={30}>最近 30 天</option>
          <option value={90}>最近 90 天</option>
        </select>
      </div>

      {report ? (
        report.days.length === 0 ? (
          <div className="settings-hint">
            这段时间还没有用量（只有真正报过 token 的回答才计入）。
          </div>
        ) : (
          <>
            <div className="settings-hint">
              合计 {formatTokens(report.totalInputTokens + report.totalOutputTokens)} token
              （输入 {formatTokens(report.totalInputTokens)} / 输出{" "}
              {formatTokens(report.totalOutputTokens)}），{report.totalMessages} 次回答
              {report.totalMessages > 0 ? "。" : "。"}
              估算花费 <b>{formatCost(report.estimatedCost)}</b>
              {report.unpricedModels.length > 0 ? "（不含下面未计价的模型）" : ""}
              。
            </div>

            <div className="usage-bars">
              {report.days.map((d) => (
                <div className="usage-bar-row" key={d.day} title={
                  `${d.day}\n输入 ${formatTokens(d.inputTokens)} / 输出 ${formatTokens(d.outputTokens)}\n` +
                  `${d.hasUnpriced ? "含未计价模型，" : ""}估算 ${formatCost(d.cost)}`
                }>
                  <span className="usage-day">{shortDay(d.day)}</span>
                  <div className="usage-bar">
                    <div
                      className="usage-bar-in"
                      style={{ width: barWidth(totalTokens(d), max) }}
                    />
                  </div>
                  <span className="usage-num">{formatTokens(totalTokens(d))}</span>
                </div>
              ))}
            </div>

            <div className="settings-hint">
              按内置价格表估算，价格会变，**仅供参考**。
              {report.unpricedModels.length > 0
                ? `以下模型不在价格表里、未计入费用：${report.unpricedModels.join("、")}。`
                : ""}
            </div>
          </>
        )
      ) : (
        <div className="settings-hint">读取中…</div>
      )}
    </div>
  );
}

/** 把文件读成 data URL；失败返回 null（不抛，让调用方跳过这个文件）。 */
function readFileAsDataUrl(file: File): Promise<string | null> {
  return new Promise((resolve) => {
    const reader = new FileReader();
    reader.onload = () =>
      resolve(typeof reader.result === "string" ? reader.result : null);
    reader.onerror = () => resolve(null);
    reader.readAsDataURL(file);
  });
}

/**
 * 知识库页：导入 / 管理 / 试检索。
 *
 * 三条设计：
 * - **导入后立刻能试检索**：用户不知道 BM25 是什么，但「我搜一下看看有没有
 *   找对」是他能判断的。所以这里放一个检索框，而不是只让他相信索引建好了。
 * - **明确告诉模型能用到它**：导入完给一句「模型在回答时会自动检索」——
 *   否则用户不知道这东西导进去有什么用。
 * - 删除是不可撤销的（会连切片与倒排一起删），所以二次确认。
 */
function KnowledgeTab() {
  const pushNotice = useUi((s) => s.pushNotice);
  const [docs, setDocs] = useState<KbDocument[]>([]);
  const [stats, setStats] = useState<KbStats | null>(null);
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<KbHit[] | null>(null);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteTitle, setPasteTitle] = useState("");
  const [pasteText, setPasteText] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);

  const refresh = async () => {
    try {
      const [list, overview] = await Promise.all([api.kbList(), api.kbStats()]);
      setDocs(list);
      setStats(overview);
    } catch (e) {
      pushNotice(`读取知识库失败：${e}`);
    }
  };

  useEffect(() => {
    void refresh();
    // 只在首次打开时拉一次；增删之后自己再调 refresh
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const importFiles = async (picked: File[]) => {
    setBusy(true);
    for (const file of picked) {
      const kind = kbFileKind(file.name);
      if (kind === null) {
        pushNotice(`「${file.name}」不能收进知识库（支持 Office 文档与常见纯文本）`);
        continue;
      }
      try {
        if (kind === "office") {
          const dataUrl = await readFileAsDataUrl(file);
          if (dataUrl === null) {
            pushNotice(`「${file.name}」读取失败`);
            continue;
          }
          await api.kbAddDocument(file.name, dataUrl);
        } else {
          const text = await file.text();
          await api.kbAddText(titleFromFileName(file.name), file.name, text);
        }
      } catch (e) {
        pushNotice(`「${file.name}」导入失败：${e}`);
      }
    }
    setBusy(false);
    await refresh();
  };

  return (
    <div className="settings-section">
      <div className="settings-hint">{describeStats(stats)}</div>

      <div className="field">
        <span>导入文档</span>
        <div className="danger-row">
          <button
            className="ghost-button"
            disabled={busy}
            onClick={() => fileInput.current?.click()}
          >
            {busy ? "处理中…" : "选择文件"}
          </button>
          <button className="ghost-button" onClick={() => setPasteOpen((v) => !v)}>
            {pasteOpen ? "取消粘贴" : "粘贴文本"}
          </button>
        </div>
        <input
          ref={fileInput}
          type="file"
          multiple
          style={{ display: "none" }}
          onChange={(e) => {
            const picked = Array.from(e.target.files ?? []);
            if (picked.length > 0) void importFiles(picked);
            // 清掉 value，否则再选同一个文件不触发 change
            e.target.value = "";
          }}
        />
        <div className="settings-hint">
          支持 Word / Excel / PPT 与 txt、md、代码、csv 等纯文本。
          导入后在聊天里打开<b>工具开关</b>，模型就能自己检索这些资料。
        </div>

        {pasteOpen ? (
          <>
            <input
              className="kb-title"
              placeholder="标题（用来在回答里标来源）"
              value={pasteTitle}
              onChange={(e) => setPasteTitle(e.target.value)}
            />
            <textarea
              className="kb-paste"
              rows={5}
              placeholder="把要收进知识库的文字粘到这里…"
              value={pasteText}
              onChange={(e) => setPasteText(e.target.value)}
            />
            <button
              className="ghost-button"
              disabled={busy || pasteText.trim().length === 0}
              onClick={async () => {
                setBusy(true);
                try {
                  await api.kbAddText(
                    pasteTitle.trim() || `粘贴的文本 ${new Date().toLocaleString()}`,
                    "",
                    pasteText,
                  );
                  setPasteText("");
                  setPasteTitle("");
                  setPasteOpen(false);
                  await refresh();
                } catch (e) {
                  pushNotice(`导入失败：${e}`);
                } finally {
                  setBusy(false);
                }
              }}
            >
              收进知识库
            </button>
          </>
        ) : null}
      </div>

      <div className="field">
        <span>试搜一下（看看索引建得对不对）</span>
        <div className="danger-row">
          <input
            className="kb-title"
            placeholder="输入关键词，例如「全局热键」"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                void api
                  .kbSearch(query, 5)
                  .then(setHits)
                  .catch((err) => pushNotice(`检索失败：${err}`));
              }
            }}
          />
          <button
            className="ghost-button"
            disabled={query.trim().length === 0}
            onClick={() =>
              void api
                .kbSearch(query, 5)
                .then(setHits)
                .catch((err) => pushNotice(`检索失败：${err}`))
            }
          >
            检索
          </button>
        </div>
        {hits !== null ? (
          hits.length === 0 ? (
            <div className="settings-hint">没有命中。换个说法试试（检索是按词匹配的）。</div>
          ) : (
            <div className="kb-hits">
              {hits.map((hit) => (
                <div className="kb-hit" key={hit.chunkId}>
                  <div className="kb-hit-head">{describeHit(hit)}</div>
                  <div className="kb-hit-text">{previewText(hit.text)}</div>
                </div>
              ))}
            </div>
          )
        ) : null}
      </div>

      <div className="field">
        <span>已收录（{docs.length}）</span>
        {docs.length === 0 ? (
          <div className="settings-hint">还没有文档。</div>
        ) : (
          <div className="kb-docs">
            {docs.map((doc) => (
              <div className="kb-doc" key={doc.id}>
                <span className="kb-doc-title" title={doc.title}>
                  {doc.title}
                </span>
                <span className="kb-doc-meta">{describeDocument(doc)}</span>
                <button
                  className="role-delete"
                  title="从知识库里删除（不可撤销）"
                  onClick={async () => {
                    if (
                      !window.confirm(
                        `从知识库删除「${doc.title}」？\n\n它的切片与索引会一起删除，不可撤销。`,
                      )
                    ) {
                      return;
                    }
                    try {
                      await api.kbDelete(doc.id);
                      await refresh();
                    } catch (e) {
                      pushNotice(`删除失败：${e}`);
                    }
                  }}
                >
                  删除
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <MemorySection />
    </div>
  );
}

/**
 * 长期记忆。
 *
 * 开关放在知识库页而不是「通用」页：记忆与知识库是同一件事的两面
 * （都是「回答时能用上的本地资料」），放在一起用户才容易理解
 * 「打开它意味着什么」。
 */
function MemorySection() {
  const config = useConfig((s) => s.config);
  const patch = useConfig((s) => s.patch);
  const pushNotice = useUi((s) => s.pushNotice);
  const [list, setList] = useState<Memory[]>([]);
  const [stats, setStats] = useState<MemoryStats | null>(null);
  const [draft, setDraft] = useState("");
  const [probe, setProbe] = useState("");
  const [hits, setHits] = useState<Memory[] | null>(null);

  const refresh = async () => {
    try {
      const [memories, overview] = await Promise.all([
        api.memoryList(),
        api.memoryStats(),
      ]);
      setList(memories);
      setStats(overview);
    } catch (e) {
      pushNotice(`读取记忆失败：${e}`);
    }
  };

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 配置还没读回来时先不画开关：显示一个「关着」的复选框会让人以为
  // 自己没打开过（其实只是还没读到）。
  if (config === null) {
    return <div className="field"><span>长期记忆</span></div>;
  }

  return (
    <div className="field">
      <span>长期记忆</span>

      <label className="field checkbox">
        <input
          type="checkbox"
          checked={config.memoryEnabled}
          onChange={(e) => void patch({ memoryEnabled: e.target.checked })}
        />
        <span>让模型记住关于我的长期信息</span>
      </label>
      <div className="settings-hint">
        打开后：模型在对话里遇到长期有用的事实时会自己记下来；你提问时，
        相关的几条会随请求**一并发给模型服务商**（所以默认关闭）。存在本机，
        可以随时在这里查看和删除。
      </div>

      <div className="settings-hint">{describeMemoryStats(stats)}</div>

      <div className="danger-row">
        <input
          className="kb-title"
          placeholder="手动加一条，例如「我偏好简洁的回答」"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && draft.trim()) {
              void api
                .memoryAdd(draft)
                .then(async (outcome) => {
                  setDraft("");
                  if (outcome.merged) pushNotice("和已有的一条几乎一样，已合并更新");
                  await refresh();
                })
                .catch((err) => pushNotice(`添加失败：${err}`));
            }
          }}
        />
        <button
          className="ghost-button"
          disabled={draft.trim().length === 0}
          onClick={() =>
            void api
              .memoryAdd(draft)
              .then(async (outcome) => {
                setDraft("");
                if (outcome.merged) pushNotice("和已有的一条几乎一样，已合并更新");
                await refresh();
              })
              .catch((err) => pushNotice(`添加失败：${err}`))
          }
        >
          记下来
        </button>
      </div>

      {list.length === 0 ? null : (
        <div className="kb-docs">
          {list.map((memory) => (
            <div className="kb-doc" key={memory.id}>
              <span className="kb-doc-title" title={memory.text}>
                {memory.text}
              </span>
              <span className="kb-doc-meta">{describeMemory(memory)}</span>
              <button
                className="role-delete"
                title="删掉这条记忆"
                onClick={() =>
                  void api
                    .memoryDelete(memory.id)
                    .then(refresh)
                    .catch((e) => pushNotice(`删除失败：${e}`))
                }
              >
                删除
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="danger-row">
        <input
          className="kb-title"
          placeholder="试试这句话会挑中哪几条记忆"
          value={probe}
          onChange={(e) => setProbe(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && probe.trim()) {
              void api
                .memoryRelevant(probe)
                .then(setHits)
                .catch((err) => pushNotice(`试搜失败：${err}`));
            }
          }}
        />
        <button
          className="ghost-button"
          disabled={probe.trim().length === 0}
          onClick={() =>
            void api
              .memoryRelevant(probe)
              .then(setHits)
              .catch((err) => pushNotice(`试搜失败：${err}`))
          }
        >
          试试
        </button>
      </div>
      {hits !== null ? (
        <div className="settings-hint">
          {hits.length === 0
            ? "这句话不会带上任何记忆。"
            : `会带上：${hits.map((m) => m.text).join("；")}`}
        </div>
      ) : null}
    </div>
  );
}

function DataTab() {
  const refreshConversations = useChat((s) => s.refreshConversations);
  const startNewConversation = useChat((s) => s.newConversation);
  const pushNotice = useUi((s) => s.pushNotice);
  const fileInput = useRef<HTMLInputElement>(null);

  const [busy, setBusy] = useState(false);
  const [exported, setExported] = useState<{
    path: string;
    bytes: number;
    conversations: number;
    messages: number;
  } | null>(null);
  const [report, setReport] = useState<{
    imported: number;
    skipped: number;
    messages: number;
  } | null>(null);

  const onExport = async () => {
    setBusy(true);
    try {
      setExported(await api.exportHistory());
    } catch (e) {
      pushNotice(`导出失败：${e}`);
    } finally {
      setBusy(false);
    }
  };

  const onPickFile = async (file: File) => {
    setBusy(true);
    setReport(null);
    try {
      // 先读成文本再交给后端：WebView 里拿不到文件的真实路径，
      // 而为了一个导入功能引入 fs 插件与权限并不划算。
      const text = await file.text();
      const result = await api.importHistory(text);
      setReport(result);
      await refreshConversations();
      pushNotice(
        `导入完成：新增 ${result.imported} 个会话、${result.messages} 条消息` +
          (result.skipped > 0 ? `，跳过 ${result.skipped} 个已存在的会话` : ""),
      );
    } catch (e) {
      pushNotice(`导入失败：${e}`);
    } finally {
      setBusy(false);
      // 清掉 input 的值，否则再选**同一个文件**不会触发 change
      if (fileInput.current) fileInput.current.value = "";
    }
  };

  return (
    <div className="settings-section">
      <UsagePanel />

      <div className="field">
        <span>导出全部历史（含图片，JSON 格式）</span>
        <button className="ghost-button" disabled={busy} onClick={() => void onExport()}>
          {busy ? "处理中…" : "导出到文件"}
        </button>
        {exported ? (
          <div className="settings-hint">
            已导出 {exported.conversations} 个会话 / {exported.messages} 条消息，
            共 {formatBytes(exported.bytes)}：
            <div className="path-row">
              <code className="path">{exported.path}</code>
              <CopyButton text={exported.path} idle="复制路径" title="复制文件路径" />
            </div>
          </div>
        ) : (
          <div className="settings-hint">
            文件会写到应用数据目录下的 <code>exports/</code> 里，文件名带时间戳。
          </div>
        )}
      </div>

      <div className="field">
        <span>从备份文件导入</span>
        <input
          ref={fileInput}
          type="file"
          accept="application/json,.json"
          disabled={busy}
          onChange={(e) => {
            const file = e.target.files?.[0];
            if (file) void onPickFile(file);
          }}
        />
        <div className="settings-hint">
          只新增，**不会覆盖**已有会话：备份里 id 已存在的会话会被跳过，
          所以同一个文件导两次也不会产生重复。
        </div>
        {report ? (
          <div className="settings-hint">
            上次导入：新增 {report.imported} 个会话、{report.messages} 条消息，
            跳过 {report.skipped} 个。
          </div>
        ) : null}
      </div>

      <div className="field">
        <span>危险操作</span>
        <div className="danger-row">
          <button
            className="danger-button"
            onClick={async () => {
              if (!window.confirm("清空全部会话历史？此操作不可撤销。")) return;
              await api.clearAllHistory();
              await startNewConversation();
              await refreshConversations();
            }}
          >
            清空全部历史
          </button>
          <button
            className="danger-button"
            title="把偏好恢复成默认值；服务商与 API Key 会保留"
            onClick={async () => {
              if (
                !window.confirm(
                  "重置全部设置？\n\n外观、提示词、角色、工具白名单、代理等会恢复默认值。\n服务商与 API Key 会保留。",
                )
              ) {
                return;
              }
              await useConfig.getState().resetSettings();
              pushNotice("设置已重置（服务商与密钥保留）");
            }}
          >
            重置设置
          </button>
          <button
            className="danger-button"
            title="从系统凭据管理器里删除全部服务商的 API Key"
            onClick={async () => {
              if (
                !window.confirm(
                  "删除全部 API Key？\n\n密钥从系统凭据管理器里删除，不可恢复，需要重新填写。",
                )
              ) {
                return;
              }
              const removed = await api.deleteAllApiKeys();
              await useProviders.getState().load();
              pushNotice(`已删除 ${removed} 个服务商的密钥`);
            }}
          >
            删除全部密钥
          </button>
        </div>
      </div>

      <DiagnosticsPanel />
    </div>
  );
}

/**
 * 诊断信息面板。
 *
 * 以前出错只能「从命令行启动 exe 看 stderr」—— 对普通用户等于没有。
 * 这里把环境与最近的错误汇总成一段可以直接复制的文本，用户反馈问题时
 * 粘给开发者就行。**刻意不含消息内容与密钥**：它天然是一条外发通道。
 */
/**
 * 开机自启。
 *
 * 勾选框显示的是**系统里的真实状态**，不是配置里的值：用户可能用任务管理器
 * 或注册表把自启项删掉了。显示配置值的话会出现「勾还打着、开机却没启动」，
 * 而用户完全不知道去哪儿查。
 *
 * 另外自启会带 `--minimized`：开机时只在后台待着，不弹悬浮窗（见 `main.tsx`）。
 */
function AutostartField() {
  const config = useConfig((s) => s.config);
  const pushNotice = useUi((s) => s.pushNotice);
  const [actual, setActual] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void api
      .autostartStatus()
      .then(setActual)
      .catch(() => setActual(null));
  }, []);

  if (!config) return null;

  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      const updated = await api.setAutostart(next);
      useConfig.setState({ config: updated });
      setActual(await api.autostartStatus().catch(() => next));
    } catch (e) {
      // 系统那边没成功就**不**改界面状态：否则会出现「勾上了、其实没生效」。
      pushNotice(`设置开机自启失败：${e}`);
      setActual(await api.autostartStatus().catch(() => null));
    } finally {
      setBusy(false);
    }
  };

  return (
    <label className="field checkbox">
      <input
        type="checkbox"
        disabled={busy}
        // 读不到系统状态（命令失败）时退回配置里的值，至少不是空白。
        checked={actual ?? config.autostart}
        onChange={(e) => void toggle(e.target.checked)}
      />
      <span>
        开机时自动启动
        {actual !== null && actual !== config.autostart
          ? "（系统里已是这个状态，与设置不一致，已按系统为准）"
          : ""}
      </span>
    </label>
  );
}

function DiagnosticsPanel() {
  const [data, setData] = useState<Diagnostics | null>(null);
  const [open, setOpen] = useState(false);
  const pushNotice = useUi((s) => s.pushNotice);

  const load = async () => {
    try {
      setData(await api.diagnostics());
    } catch (e) {
      pushNotice(`读取诊断信息失败：${e}`);
    }
  };

  useEffect(() => {
    if (open && !data) void load();
    // data 不进依赖：只在首次展开时拉一次，之后的刷新靠「重新读取」按钮。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  return (
    <div className="field">
      <span>诊断信息</span>
      <div className="danger-row">
        <button className="ghost-button" onClick={() => setOpen((v) => !v)}>
          {open ? "隐藏" : "显示诊断信息"}
          {!open && hasProblems(data) ? "（有错误记录）" : ""}
        </button>
        {open ? (
          <>
            <button className="ghost-button" onClick={() => void load()}>
              重新读取
            </button>
            <button
              className="ghost-button"
              onClick={async () => {
                const removed = await api.clearLogs();
                await load();
                pushNotice(`已清空 ${removed} 条日志`);
              }}
            >
              清空日志
            </button>
          </>
        ) : null}
      </div>

      {open ? (
        data ? (
          <div className="diagnostics">
            <div className="path-row">
              <CopyButton
                text={formatDiagnostics(data)}
                idle="复制诊断信息"
                title="复制成一段可直接粘贴的文本"
              />
            </div>
            <pre className="diagnostics-text">{formatDiagnostics(data)}</pre>
          </div>
        ) : (
          <div className="settings-hint">读取中…</div>
        )
      ) : null}
    </div>
  );
}

function AboutTab() {
  const info = useConfig((s) => s.info);
  const [totals, setTotals] = useState({ inputTokens: 0, outputTokens: 0 });

  useEffect(() => {
    void api
      .tokenTotals()
      .then(setTotals)
      .catch(() => undefined);
  }, []);

  return (
    <div className="settings-section">
      <div className="field">
        <span>版本</span>
        <div className="settings-hint">{info?.version ?? "–"}</div>
      </div>
      <div className="field">
        <span>累计 token 用量</span>
        <div className="settings-hint">
          输入 {totals.inputTokens} · 输出 {totals.outputTokens}
        </div>
      </div>
      <div className="field">
        <span>数据库</span>
        <div className="settings-hint path">{info?.dbPath ?? "–"}</div>
      </div>
      <div className="field">
        <span>配置文件</span>
        <div className="settings-hint path">{info?.configPath ?? "–"}</div>
      </div>
      <p className="settings-note">
        所有会话历史、模型配置都在本机；API Key 在 Windows 凭据管理器。
        除了你自己配置的模型服务商，不请求任何第三方服务。
      </p>
    </div>
  );
}
