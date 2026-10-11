import { describe, expect, it } from "vitest";

import { toolLabel } from "./tools";

describe("toolLabel", () => {
  it("已知工具翻成中文", () => {
    expect(toolLabel("fetch_webpage")).toBe("抓取网页");
    expect(toolLabel("write_clipboard_text")).toBe("写入剪贴板");
  });

  it("未知工具原样返回，不会显示成空白", () => {
    // 后端加了新工具、前端还没跟上时，界面至少要能看出是哪个工具。
    expect(toolLabel("brand_new_tool")).toBe("brand_new_tool");
  });
});
