// MCP 显示逻辑的测试。
//
// 守的是「状态显示得让人误解」这一类问题：连不上却显示成「已连接 0 个工具」、
// 校验漏掉重复 id（后端会拒，但用户得先看到原因）。

import { describe, expect, it } from "vitest";

import type { McpServerConfig, McpServerStatus } from "../types";
import {
  blankMcpServer,
  formatArgs,
  mergeServerEdit,
  parseArgs,
  describeMcpCommand,
  describeMcpStatus,
  validateMcpServer,
} from "./mcp";

const status = (over: Partial<McpServerStatus> = {}): McpServerStatus => ({
  id: "s1",
  name: "文件服务",
  connected: true,
  serverName: "fake-mcp",
  serverVersion: "9.9.9",
  tools: [],
  error: null,
  ...over,
});

const server = (over: Partial<McpServerConfig> = {}): McpServerConfig => ({
  id: "s1",
  name: "文件服务",
  command: "npx",
  args: ["-y", "server-filesystem", "/tmp"],
  env: {},
  enabled: true,
  ...over,
});

describe("describeMcpStatus", () => {
  it("连不上时显示原因", () => {
    const line = describeMcpStatus(status({ connected: false, error: "找不到命令 npx" }));
    expect(line).toContain("连不上");
    expect(line).toContain("找不到命令");
  });

  it("连上了但没工具时说清楚 —— 不能显示成「0 个工具」了事", () => {
    const line = describeMcpStatus(status({ tools: [] }));
    expect(line).toContain("没有提供任何工具");
  });

  it("正常时给出工具数与服务端自称的名字", () => {
    const line = describeMcpStatus(
      status({
        tools: [
          { rawName: "echo", name: "mcp__fake__echo", description: "" },
          { rawName: "read", name: "mcp__fake__read", description: "" },
        ],
      }),
    );
    expect(line).toContain("2 个工具");
    expect(line).toContain("fake-mcp");
  });

  it("服务端没自报名字时不留一对空括号", () => {
    const line = describeMcpStatus(status({ serverName: null, serverVersion: null }));
    expect(line).not.toContain("（）");
  });
});

describe("describeMcpCommand", () => {
  it("拼出命令与参数", () => {
    expect(describeMcpCommand(server())).toBe("npx -y server-filesystem /tmp");
  });

  it("空参数不留多余空格", () => {
    expect(describeMcpCommand(server({ args: ["", "  ", "-y"] }))).toBe("npx -y");
  });
});

describe("validateMcpServer", () => {
  it("名字与命令必填", () => {
    expect(validateMcpServer(server({ name: "  " }), [server()])).toContain("名字");
    expect(validateMcpServer(server({ command: "" }), [server()])).toContain("命令");
  });

  it("id 重复要拦下来（后端也会拒，但用户得当场看到原因）", () => {
    const duplicated = [server(), server({ name: "另一个" })];
    expect(validateMcpServer(duplicated[0], duplicated)).toContain("重复");
  });

  it("正常配置通过", () => {
    expect(validateMcpServer(server(), [server()])).toBeNull();
  });
});

describe("blankMcpServer", () => {
  it("给一个能直接跑的起点（文件系统服务），而不是空表单", () => {
    const blank = blankMcpServer(2);
    expect(blank.command).toBe("npx");
    expect(blank.args.join(" ")).toContain("server-filesystem");
    expect(blank.name).toContain("2");
    expect(blank.enabled).toBe(true);
    // id 必须唯一，否则第二个就存不进去
    expect(blankMcpServer(1).id).not.toBe(blankMcpServer(2).id);
  });
});

describe("parseArgs：参数里的空格", () => {
  it("普通参数按空白拆", () => {
    expect(parseArgs("-y server-filesystem /tmp")).toEqual(["-y", "server-filesystem", "/tmp"]);
  });

  it("带空格的路径用引号包起来就是一个参数", () => {
    // 这不是假想：应用自己的工作区就叫「桌面 AI 聊天助手应用」，
    // 第一版直接 split 时，这条路径被拆成三段，MCP 服务端起不来。
    expect(parseArgs('"D:\\项目\\桌面 AI 聊天助手应用\\server.mjs"')).toEqual([
      "D:\\项目\\桌面 AI 聊天助手应用\\server.mjs",
    ]);
    expect(parseArgs('-y "C:\\Program Files\\x\\y.mjs" --flag')).toEqual([
      "-y",
      "C:\\Program Files\\x\\y.mjs",
      "--flag",
    ]);
    expect(parseArgs("'单引号 也行'")).toEqual(["单引号 也行"]);
  });

  it("多余的空格不会产生空参数", () => {
    expect(parseArgs("   -y    /tmp   ")).toEqual(["-y", "/tmp"]);
    expect(parseArgs("")).toEqual([]);
    expect(parseArgs("   ")).toEqual([]);
  });

  it("不成对的引号：剩下的整体当一个参数，而不是静默丢掉", () => {
    expect(parseArgs('"没闭合 的路径')).toEqual(["没闭合 的路径"]);
  });

  it("空引号是一个空参数（用户明确写了就要保留）", () => {
    expect(parseArgs('a "" b')).toEqual(["a", "", "b"]);
  });

  it("formatArgs 会把含空格的参数加回引号，两者可往返", () => {
    const args = ["-y", "C:\\Program Files\\x.mjs", "/tmp"];
    const text = formatArgs(args);
    expect(text).toContain('"C:\\Program Files\\x.mjs"');
    expect(parseArgs(text)).toEqual(args);
  });
});

describe("mergeServerEdit：并发编辑不互相覆盖", () => {
  const a = { id: "a", name: "甲", command: "node", args: [], env: {}, enabled: true };
  const b = { id: "b", name: "乙", command: "npx", args: [], env: {}, enabled: true };

  it("只搬真正变了的字段 —— 用户先改名字、再改命令，两次都不该丢", () => {
    // 场景：旧快照 [a]，第一次改名字 → next1；还没回来时第二次改命令 → next2（也是按 [a] 拼的）
    const next1 = [{ ...a, name: "甲改名" }];
    const next2 = [{ ...a, command: "uvx" }];
    // 第一次落地
    const afterFirst = mergeServerEdit([a], next1, [a]);
    expect(afterFirst[0]).toMatchObject({ name: "甲改名", command: "node" });
    // 第二次基于**最新**状态合并：名字保住了，命令也改上了
    const afterSecond = mergeServerEdit([a], next2, afterFirst);
    expect(afterSecond[0]).toMatchObject({ name: "甲改名", command: "uvx" });
  });

  it("新增与删除照常", () => {
    const added = mergeServerEdit([a], [a, b], [a]);
    expect(added.map((s) => s.id)).toEqual(["a", "b"]);
    const removed = mergeServerEdit([a, b], [a], [a, b]);
    expect(removed.map((s) => s.id)).toEqual(["a"]);
  });

  it("最新列表里多出来的服务器不会被误删", () => {
    // 别的窗口/别的地方刚加了一个，我的补丁不该把它带走
    const c = { id: "c", name: "丙", command: "node", args: [], env: {}, enabled: true };
    const result = mergeServerEdit([a], [{ ...a, name: "改名" }], [a, c]);
    expect(result.map((s) => s.id).sort()).toEqual(["a", "c"]);
    expect(result.find((s) => s.id === "a")?.name).toBe("改名");
  });

  it("数组与对象字段的变化也能识别", () => {
    const result = mergeServerEdit([a], [{ ...a, args: ["-y", "x"] }], [a]);
    expect(result[0].args).toEqual(["-y", "x"]);
  });
});
