import { useProviders } from "../stores/providers";
import { useUi } from "../stores/ui";

/**
 * 输入框上方的模型选择器。
 *
 * 只改「默认服务商 + 默认模型」，改动立刻落盘：下一个会话和下次启动都用它。
 * 按会话记住模型是下一版的事（`conversations.provider_id / model_key` 字段
 * 已经留好了）。
 */
export function ModelPicker() {
  const state = useProviders((s) => s.state);
  const setDefault = useProviders((s) => s.setDefault);
  const pushNotice = useUi((s) => s.pushNotice);

  if (!state || state.providers.length === 0) {
    return <span className="picker-empty">未配置模型</span>;
  }

  const provider =
    state.providers.find((p) => p.id === state.defaultProviderId) ??
    state.providers[0];

  return (
    <div className="model-picker">
      <select
        value={provider.id}
        onChange={(e) => {
          const next = state.providers.find((p) => p.id === e.target.value);
          if (!next) return;
          if (next.models[0]) {
            void setDefault(next.id, next.models[0].key);
            return;
          }
          // 这个服务商一个模型都没有。以前这里什么都不做，受控的 <select>
          // 会立刻弹回原来的选项 —— 看起来就是「下拉框坏了」。
          pushNotice(
            `「${next.name}」还没有添加模型，请到「设置 → 模型」里加一个。`,
          );
        }}
      >
        {state.providers.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name}
            {p.hasKey ? "" : "（未配 Key）"}
          </option>
        ))}
      </select>

      <select
        value={
          provider.models.some((m) => m.key === state.defaultModelKey)
            ? (state.defaultModelKey as string)
            : (provider.models[0]?.key ?? "")
        }
        onChange={(e) => void setDefault(provider.id, e.target.value)}
      >
        {provider.models.length === 0 ? (
          <option value="">（无模型）</option>
        ) : null}
        {provider.models.map((m) => (
          <option key={m.key} value={m.key}>
            {m.key}
          </option>
        ))}
      </select>
    </div>
  );
}
