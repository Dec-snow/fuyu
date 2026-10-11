// 模型选择器的**渲染级**测试：收藏到底有没有体现在界面上。
//
// 这是「数据字段早就有了、界面上却一直没有入口」那个缺口的回归测试 ——
// 光有 `sortModels` 的单测还不够，得证明**选择器真的用了它**。
//
// ⚠️ 必须把 store mock 掉：SSR 下 zustand 走 `getServerSnapshot`，读到的永远是
// **初始**状态（实测：`setState` 之后 `getState()` 有数据，组件里读到的仍是
// null）。mock 成普通函数之后，服务端渲染才拿得到我们准备的夹具。
//
// 夹具放在 `vi.hoisted` 的可变容器里：每个用例改一改就能渲染不同场景，
// 不用 `resetModules` + 动态 import 那一套（那套在 mock 已生效时并不可靠）。

import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { holder } = vi.hoisted(() => ({
  holder: { state: null as unknown },
}));

vi.mock("../stores/providers", () => ({
  useProviders: (selector: (s: unknown) => unknown) =>
    selector({ state: holder.state, setDefault: () => {} }),
}));
vi.mock("../stores/ui", () => ({
  useUi: (selector: (s: unknown) => unknown) => selector({ pushNotice: () => {} }),
}));

import { ModelPicker } from "./ModelPicker";

/** 一个服务商 + 三个模型，中间那个是收藏的。 */
function stateWithFavorites() {
  return {
    providers: [
      {
        id: "p1",
        name: "本地假模型",
        baseUrl: "http://127.0.0.1:8791/v1",
        isPreset: false,
        hasKey: false,
        models: [
          { key: "model-plain", favorite: false },
          { key: "model-starred", favorite: true },
          { key: "model-also-plain", favorite: false },
        ],
      },
    ],
    defaultProviderId: "p1",
    defaultModelKey: "model-plain",
  };
}

describe("ModelPicker 里的收藏", () => {
  beforeEach(() => {
    holder.state = stateWithFavorites();
  });

  it("收藏的模型带 ★ 前缀，且排在其他模型前面", () => {
    const html = renderToStaticMarkup(<ModelPicker />);

    expect(html).toContain("★ model-starred");
    // 未收藏的保持原样，不能也带上星标
    expect(html).toContain(">model-plain<");
    expect(html).not.toContain("★ model-plain");

    // 「排在前面」用 DOM 顺序验：<option> 的先后就是下拉框里的先后
    const starred = html.indexOf("★ model-starred");
    const plain = html.indexOf(">model-plain<");
    const alsoPlain = html.indexOf(">model-also-plain<");
    expect(starred).toBeGreaterThan(-1);
    expect(starred).toBeLessThan(plain);
    expect(plain).toBeLessThan(alsoPlain);
  });

  it("默认模型仍然被选中（收藏不该顺手动默认模型）", () => {
    const html = renderToStaticMarkup(<ModelPicker />);
    expect(html).toContain('value="model-plain"');
  });

  it("一个都没收藏时顺序保持原样", () => {
    holder.state = {
      ...stateWithFavorites(),
      providers: [
        {
          ...stateWithFavorites().providers[0],
          models: [
            { key: "a", favorite: false },
            { key: "b", favorite: false },
          ],
        },
      ],
    };
    const html = renderToStaticMarkup(<ModelPicker />);
    expect(html.indexOf(">a<")).toBeLessThan(html.indexOf(">b<"));
    expect(html).not.toContain("★");
  });

  it("没有服务商时给一句明确提示，而不是一个空下拉", () => {
    holder.state = { providers: [], defaultProviderId: null, defaultModelKey: null };
    const html = renderToStaticMarkup(<ModelPicker />);
    expect(html).toContain("未配置模型");
  });

  it("没配 Key 的服务商要标出来（否则用户不知道为什么会发不出去）", () => {
    const html = renderToStaticMarkup(<ModelPicker />);
    expect(html).toContain("（未配 Key）");
  });
});
