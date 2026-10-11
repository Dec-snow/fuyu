# 浮语 FloatAI · 桌面悬浮窗 AI 助手

> 一个用 **Rust + Tauri 2** 从零写出来的桌面 AI 助手：`Alt+Space` 唤出、问完即走，
> 数据全在本机，可接任何 Chat Completions 兼容模型。
>
> 本项目是**学习项目**：参考 [ZHIMA-agent](https://github.com/MOONKEY0510/ZHIMA-agent)
> 的架构，按自己的理解重新实现了一遍核心链路，并逐模块写下「为什么这么做」。

---

## 这是什么

一个常驻桌面的 AI 助手，不是「又一个聊天客户端」：

- **随唤随到**：`Alt+Space` 全局唤起 / 隐藏，失焦自动隐藏，位置可记忆；
- **不绑定厂商**：任何 Chat Completions 兼容接口 —— DeepSeek / 智谱 / Kimi / 通义 /
  MiniMax / 硅基流动 / OpenAI / OpenRouter / 本地 Ollama，内置预设一键可用；
- **数据留在本机**：会话历史在本地 SQLite，API Key 在 Windows 凭据管理器；
- **小体积**：Tauri 2 + 系统 WebView2，不打包 Chromium —— 安装包 **2.07 MB**。

## 已实现（MVP）

| 模块 | 内容 |
|---|---|
| 悬浮窗 | 无边框 / 透明 / 置顶 / 跳过任务栏 / 圆角 / 水浮现入场动效 |
| 完整模式 | 一键从悬浮窗展开为带历史侧栏的完整窗口，尺寸与模式持久化 |
| 全局热键 | `Alt+Space` 唤出与隐藏，冲突时明确报错而非静默失败 |
| 系统托盘 | 左键开合窗口、右键菜单、退出；单实例（第二次启动只唤起已有窗口） |
| 多服务商多模型 | 9 个内置预设 + **可自己增删**自定义服务商；模型手动添加或 `GET /models` 拉取 |
| 流式对话 | Rust 直连 + 健壮的 SSE 解析（粘包 / 拆包 / UTF-8 跨块 / usage 宽限期） |
| 上下文预算 | 估算 token，超出时从最旧的消息开始省略，并在回答上标注省略条数 |
| 思考过程 | 识别 `reasoning_content` / `reasoning` / `thinking` 三种字段并折叠展示 |
| 停止生成 | 基于 `CancellationToken`，已生成内容照常落库 |
| 历史会话 | 新建 / 重命名 / 置顶 / 删除 / 清空；首条消息自动命名；列表可分页加载 |
| 全文搜索 | SQLite **FTS5 trigram**，中文按子串匹配，短词自动回退 `LIKE`；点命中项会滚到并高亮那条消息 |
| 设置面板 | 模型 / 外观 / 通用 / 关于 四个页签 |
| 密钥安全 | Key 只存系统凭据管理器，前端只能看到 `hasKey: bool` |
| 代理 | 显式 HTTP / SOCKS5 代理，或跟随系统与环境变量代理；改完即时生效（无需重启） |
| token 统计 | 累计输入 / 输出 token |
| **失败可重试** | 失败与取消的消息也落库（带错误码与「重试」按钮），重开应用还能重试 |
| **长会话虚拟列表** | `react-virtuoso` 只渲染视口内的气泡；往上翻历史时不被流式输出拽回底部 |
| **消息编辑重发** | 双击用户气泡改内容 → 该消息之后的旧回答全部删掉 → 用新历史重跑 |
| **Agent 工具系统** | 原生 function calling 循环（最多 5 轮），工具：时间 / 计算器 / 剪贴板读写 / 网页抓取 / 联网搜索 |
| **三级授权** | 敏感工具执行前弹审批卡：允许一次 / 本次会话 / 永久（写入配置）；外发型工具**不允许**永久 |
| **数据流防泄漏** | 读过本地敏感数据后，上下文再发给模型服务商前必须再确认一次 |
| **划词助手 `Alt+Q`** | 保存剪贴板 → 模拟 Ctrl+C → 轮询 → 还原；在目标程序里选中文字即可取词提问 |
| **滚动摘要** | 会话变长后自动把最早的一段历史压成摘要（增量合并），而不是直接丢掉 |
| **安全抓取（SSRF 防护）** | 只允许 http(s)、DNS 每个 IP 都必须是公网、钉死 IP 防 rebinding、重定向逐跳校验、响应限长 |
| **联网搜索** | 配置 SearXNG 端点后启用；未配置时该工具**不下发**给模型 |
| **图片输入** | 粘贴或拖入图片（png/jpeg/webp/gif）→ 多模态消息发给视觉模型；图片存库、按 id 回填，不走 IPC |
| **错误不再静默** | 四路错误/通知（聊天 / 设置 / 服务商 / 系统通知）统一渲染成提示条；以前它们只写进 store、没有任何组件读 |
| **永久授权可撤销** | 审批卡上给出的「永久允许」能在「设置 → 通用」里看到并逐项撤销（以前只能手工改 `providers.json`） |
| **一键复制** | 每条消息悬停出现「复制」；代码块头部另有「复制代码」。走 Rust 的 `arboard` 而不是浏览器剪贴板 API（后者在悬浮窗里会静默失败），失败时按钮当场变「复制失败」 |
| **模型收藏** | 星标常用模型，输入框上方的下拉里 `★` 前缀 + 排到最前（只影响展示顺序，不会顺手动默认模型） |
| **窗口透明度 / 正文字号** | 透明度只作用于背景（文字始终清晰），设置面板不跟着透明；字号影响聊天内容与输入框。两者都有后端硬边界（不透明度下限 0.35），拖动时即时预览 |
| **角色预设（人设）** | 4 个内置模板（翻译官 / 代码助手 / 文案写手 / 严谨总结）+ 自定义；**按会话**在输入框上方切换，人设优先于全局系统提示词。内置模板可改不可删，空名字/空提示词在后端被拒 |
| **备份导出 / 导入** | 「设置 → 数据」：导出全部历史（含图片）为带版本号的 JSON；导入**只新增不覆盖**（id 已存在的会话跳过），同一个文件导两次不会重复 |

## 未实现（留给下一版）

MCP 客户端、本地知识库（BM25）、长期记忆、技能（Skill）、
文档解析（docx / xlsx / pptx / pdf）、文生图、一问多答、
Markdown 高级渲染（KaTeX / mermaid / 代码高亮）、数据备份导入导出、自动更新。

这些功能在参考项目里都有实现。作者对参考项目的逐层拆解笔记（含原样摘抄的代码）
**只保留在本地，没有随本仓库发布**，原因见文末「致谢与来源说明」；
`docs/02-学习路线.md` 里保留了那四篇的索引。

---

## 快速开始

### 前置

| 依赖 | 说明 |
|---|---|
| Node.js ≥ 18 | 前端构建 |
| Rust stable（MSVC） | `rustup default stable-x86_64-pc-windows-msvc` |
| Visual Studio 生成工具 | 需要「使用 C++ 的桌面开发」工作负载 |
| WebView2 Runtime | Windows 11 自带；Win10 可能需要单独装 |

### 开发

```bash
npm install
npm run tauri:dev
```

`npm run tauri:dev` 会同时拉起 Vite（`localhost:1420`）和 Tauri 应用。

> **注意**：`tauri.conf.json` 里的 `frontendDist` 指向 `../dist`，而 `tauri_build`
> 在**编译期**就要求这个目录存在。所以在跑 `cargo check` / `cargo build` 之前，
> 必须先执行过一次 `npm run build`（`npm run tauri:dev` 会自动处理）。
> 单独跑 `cargo` 报 `The `frontendDist` configuration is set to ... but this path doesn't exist`
> 就是这个原因。
>
> ### ⚠️ 构建二进制只走 `npm run tauri:build`
>
> 这一条是**实测踩出来的**，不是理论推断：**直接用 `cargo build --release` 打出来的
> `floatai.exe` 是不能用的**。
>
> 现象（很有迷惑性，值得记住）：进程起来了、WebView2 也在跑、窗口对象也存在，
> 但前端**一个 IPC 都不发**（`get_config` / `list_conversations` / `show_window`
> 全都不会被调用），于是窗口永远停在 `visible: false` + `opacity: 0` 的初始态 ——
> 用户看到的是「点了图标什么都没发生」，控制台也没有任何错误。
>
> 原因：前端资源是 **`tauri-build`（`build.rs`）+ `tauri::generate_context!`** 这两步
> 一起嵌进二进制的，Tauri CLI 会保证这两步按正确顺序、带着正确的上下文跑完。
> 裸 `cargo build` 不做这件事。**碰 `lib.rs` 或 `build.rs` 的 mtime 也不管用**
> （两种都试过：CLI 构建正常，裸 cargo 构建照样是坏的）。
>
> 所以：**要跑起来的二进制，一律 `npm run tauri:build`。**
> `cargo build` / `cargo test` / `cargo clippy` 只用于类型检查、单测和静态检查 ——
> 它们不需要一个能跑的前端。
>
> ### ⚠️ 不要在 `index.html` 里写内联 `<script>`
>
> `tauri.conf.json` 的 CSP 是 `script-src 'self'`。内联脚本**会被拒绝执行**，
> 而且实测的后果不是「少一个功能」而是**整个页面都不工作**（Tauri 会把内联脚本的
> 哈希注进 CSP，这段处理一旦出偏差，模块脚本也一起被拦掉）：
> 表现同样是那个「透明且不显示」的窗口。
>
> 所以所有需要在渲染前装好的钩子（全局报错、启动失败兜底）都写在 `src/main.tsx`
> 模块里 —— 模块属于 `'self'`，不受影响。`index.html` 保持干净。
>
> ### 排查「窗口不显示」的三步
>
> 1. `IsWindowVisible(hwnd)` 是 `true` 还是 `false`（`false` = 前端没走到 `show_window`）；
> 2. 在 `get_config` / `show_window` 里临时 `eprintln!`，看前端到底有没有发 IPC ——
>    **一个都没有**说明 JS 根本没跑起来（多半是资源没嵌进去，见上一条）；
> 3. 前端的 `data-phase` 只有切到 `shown` 才会 `opacity: 1`；所以任何启动期的异常
>    都会留下一个**完全透明**的窗口。`src/main.tsx` 里的 `showBootFailure()`
>    就是为此存在的：它把 phase 直接设成 `shown` 并把错误画在窗口上，
>    免得「白屏」变成没法调试的故障。

### 构建安装包

```bash
npm run tauri:build
```

产物在 `src-tauri/target/release/bundle/nsis/`。

### 手动测试（不需要任何 API Key）

```bash
npm run mock                  # 起一个本地假服务商（Node，零额外依赖）
```

然后在「设置 → 模型 → 添加自定义服务商」里填：

| 字段 | 值 |
|---|---|
| Base URL | `http://127.0.0.1:8791/v1` |
| API Key | **留空**（地址含 `127.0.0.1` 会被判为本地服务，允许无 Key） |
| 模型 | `mock-model` |

假服务端会**把应用真正发出去的请求逐条打印出来**（角色序列、有没有带 `tools`、
系统提示词、回灌的工具结果）—— 界面「看起来在转圈」时，这份日志就是
「前端没发」还是「后端没回」的分界线。

按关键词控制返回形状：

| 发这句话 | 假服务端的行为 | 用来验证 |
|---|---|---|
| （任意） | 分片回答，中间夹一个 usage 帧 | 流式渲染、token 统计 |
| `慢` | 20 片 × 1 秒 | **停止生成**：点停止后那半截回答必须落库（切走再切回来还在） |
| `工具` | `tool_calls: read_clipboard_text` | 敏感工具审批卡（4 颗按钮）、永久允许写盘 |
| `时间` | `tool_calls: get_current_time` | 非敏感工具**不该**弹卡片，直接执行 |
| `计算` | `tool_calls: calculator(12*34)` | 工具参数传递、结果回灌 |
| `抓取` | `tool_calls: fetch_webpage(URL)` | 外发型工具：每次都要审批，卡片上要能看到 URL |
| `摘要` | 非流式返回一段假摘要 | 滚动摘要链路 |

**测之前建议先关掉「设置 → 通用 → 失焦自动隐藏」**（默认开着）。
它是这个应用的正常行为（悬浮助手点别处就收起），但手动逐项测的时候
会让你不断找不到窗口。

其他两个不用 Key 的选项：装 [Ollama](https://ollama.com) 用本地模型
（预设里就有，同样免 Key），或者直接在「模型」里填一个真实服务商的 Key。

### 检查

```bash
npm run build                 # tsc --noEmit + vite build
npm test                      # vitest 前端单元测试
cd src-tauri && cargo test    # Rust 单元测试 + 本地假 SSE 服务端集成测试
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

### 已验证状态

本项目在目标机器上实际跑过以下检查，全部通过：

| 检查 | 结果 |
|---|---|
| `cargo test` | **206 passed / 0 failed**（不联网；含真开 TCP 端口的流式 / 抓取 / 搜索集成测试，**真读写系统剪贴板**的往返测试，以及**备份导出→导入**的往返测试） |
| `cargo clippy --all-targets -- -D warnings` | 通过，0 warning |
| `cargo fmt --check` | 通过 |
| `npm test`（vitest） | **120 passed / 0 failed**（含 11 条直接驱动 store 的竞态/接线测试，以及组件级渲染断言） |
| `npm run build`（tsc + vite） | 通过，产物 267 KB JS / 16.4 KB CSS |
| debug 二进制启动 | 进程存活，无 panic，无 stderr 输出 |
| **旧版配置兼容** | 用「加字段之前写下的 `providers.json`」启动，进程正常存活（新字段回落默认值） |
| **非 UTF-8 配置** | 用 UTF-16LE 写的 `providers.json` → 原文件被**留档**（`.corrupt-<时间戳>.json`）而不是被覆盖 |
| **数据库迁移** | 真实旧库 `user_version` 6 → 8 → 10 → 11 → 12 逐级升上去；另有「从 v11 带数据的库原地升级」测试，断言旧数据一条不丢 |
| **SSRF 防护** | 云元数据 / 回环 / 私网 / 内网域名 / `file:` / IPv6 多种写法（含 IPv4-compatible `::127.0.0.1`）均在发请求前被拒 |
| **图片白名单** | `data:text/html`、`data:image/svg+xml`（可执行脚本）在落库前被拒 |
| 首次启动副作用 | 生成 `floatai.db`（`user_version=12`、`journal_mode=wal`）、`providers.json`（9 个预设，**不含任何密钥字段**） |
| release 二进制 | `floatai.exe` **6.04 MB**，启动正常，窗口尺寸与配置一致 |
| `npm run tauri:build` | 通过；NSIS 安装包 `FloatAI_0.1.0_x64-setup.exe` **2.17 MB** |

> 安装包 2.07 MB 是「Tauri 2 + 系统 WebView2，不打包 Chromium」的直接结果
> （参考项目 6.4 MB，功能多得多）。

---

## 项目结构

```text
src/                          React + TypeScript 前端
  App.tsx                     布局与失焦隐藏策略
  main.tsx                    启动顺序：先接事件桥 → 再拉数据 → 最后 show 窗口
  types.ts                    与 Rust 一一对应的类型（手写，靠 tsc 兜住改名）
  lib/                        纯逻辑（不依赖 React / Tauri，可直接单测）
    markdown.ts               Markdown 切块与行内片段解析
    event-gate.ts             流式事件按 seq 去重
    chat-state.ts             消息工厂 + 事件折叠（在飞消息与会话的映射）
  services/
    api.ts                    所有 invoke 的唯一入口
    events.ts                 唯一的 Tauri 事件监听器
  stores/                     Zustand：config / providers / chat / ui
  components/                 TitleBar / MessageList / Composer / Sidebar /
                              SettingsPanel / ModelPicker / Markdown
  styles/globals.css          设计令牌（浅色 / 深色）+ 全部样式

src-tauri/src/                Rust 后端
  lib.rs                      应用装配：插件 → state → 命令 → setup
  main.rs                     入口（release 下隐藏控制台窗口）
  errors.rs                   AppError{code,message,retryable} + 带上限读 body
  state.rs                    AppState：HTTP 客户端 + 取消令牌表
  agent/
    context.rs                上下文预算：token 估算 + 从最旧开始裁剪 + 摘要装配
    summary.rs                滚动摘要的纯逻辑：该不该摘、折叠哪一段、提示词怎么拼
  tools/
    registry.rs               工具元数据 + 执行分发 + 转 OpenAI tools（策略字段不下发）
    web.rs                    SSRF 安全抓取 + HTML 转文本 + SearXNG 搜索
  models/
    chat.rs                   ChatMessage / ChatEvent / Conversation / MessageHit
    config.rs                 AppInfo + 9 个内置服务商预设
  storage/
    mod.rs                    now_ms / new_id
    config.rs                 providers.json（原子写入 + 损坏留档）+ AppConfig
    secrets.rs                Windows 凭据管理器封装
    database.rs               SQLite：11 步迁移、CRUD、FTS5 中文搜索、滚动摘要列、图片列
  api/                        协议层（全部纯函数，可离线单测）
    sse.rs                    增量 SSE 解析器（字节层）
    openai.rs                 Chat Completions 适配器（语义层，含多模态 content 数组）
    stream_client.rs          重试策略 + 流式消费核心（含工具调用累积）
    complete.rs               非流式一次性补全（滚动摘要 / 连接测试共用）
  commands/                   Tauri 管道层（薄，只做校验 / 装配 / 广播）
    chat.rs                   一轮对话：裁剪 → agent 循环（工具 + 审批）→ 落库 → 摘要
    history.rs                会话与消息 CRUD（含图片白名单校验）
    providers.rs              服务商与模型管理、Key 存取
    settings.rs               全局设置
    window.rs                 窗口命令（含退场动画兜底 + 快捷键重注册）
  window/
    manager.rs                显示 / 隐藏 / 模式切换 / 位置记忆
    shortcuts.rs              全局热键注册（唤醒 + 划词）与冲突上报
    quick_action.rs           划词取词：保存剪贴板 → 模拟 Ctrl+C → 轮询 → 还原
    tray.rs                   托盘图标与菜单

docs/
  analysis/                   参考项目 ZHIMA-agent 的逐层拆解（4 篇，约 60 万字符）
  01-架构设计.md               本项目的设计决策与取舍
  02-学习路线.md               怎么读这份代码、动手练习、下一步做什么
scripts/gen_icons.py          用 Pillow 生成应用与托盘图标
```

---

## 与参考项目的对应关系

| 维度 | ZHIMA-agent | 本项目 |
|---|---|---|
| 后端规模 | 约 1.5 万行 Rust，99 个命令 | 约 9,500 行 Rust（含 161 个测试），40 个命令 |
| 数据库 | 12 张表，迁移 v1→v22 | 2 张表 + 1 张 FTS5，迁移 v1→v11（幂等加列） |
| 命令错误 | `Result<T, String>` | `Result<T, AppError{code,message,retryable}>` |
| 中文检索 | FTS5 trigram | 相同（这是参考项目最值得抄的一处） |
| SSE 解析 | 字节层 / 语义层两层解耦 | 相同，并补了「每个字节位置切开都对」的属性测试 |
| 流式消费 | 与 Tauri 命令耦合在一起 | 抽出 `api/stream_client.rs`，可对着本地假服务端端到端测 |
| 上下文预算 | `fit_context` 在知识库/技能注入**之前**执行（真实缺陷） | 在装配请求体前统一裁剪，并把「丢弃 / 已摘要」条数分别告诉前端 |
| 数据库打开 | 只设 `foreign_keys` | 补上 `journal_mode=WAL` + `busy_timeout` |
| 迁移加列 | 匹配 `duplicate column name` 错误串 | 先查 `PRAGMA table_info`，幂等且不依赖英文错误串 |
| 助手消息落库 | 插消息 + 顶会话时间分两条语句 | 一个事务（`append_assistant_turn`） |
| 配置损坏 | 解析失败即回落默认值 | 先把损坏文件改名留档（`.corrupt-<时间戳>.json`）再回落，并容忍 UTF-8 BOM |
| 工具系统 | `Tool` trait + `Box<dyn>` | 纯数据 `ToolDefinition` + `match` 分发（trait 在这里没收益） |
| SSRF 防护 | 见 `docs/analysis/` | 六道闸门：协议白名单 / 全部 DNS 记录校验 / 钉死 IP / 手动跟重定向 / 限长 / 超时 |
| 前端 | React + Tailwind + 7 个 store + 虚拟列表 | React + 手写 CSS + 4 个 store + 3 个纯逻辑模块 + `react-virtuoso` |
| 图片输入 | 见 `docs/analysis/` | 存本机 SQLite + data URL 白名单；图片按消息 id 回填，不走 IPC |
| MCP / 知识库 | 完整实现 | 未实现（见 `docs/analysis/` 拆解） |

---

## 隐私

- 会话历史、模型配置保存在本机（SQLite + `providers.json`）；
- API Key 保存在 Windows 凭据管理器，**不写入配置文件、不回传前端、不写日志**；
- 除了你自己配置的模型服务商，不请求任何第三方服务。

启用**工具**后的额外说明（默认关闭，需要你主动打开输入框上的「工具开关」）：

- 敏感工具（剪贴板读写、联网抓取、联网搜索）执行前一定会弹审批卡，
  卡片上显示**完整参数**（联网工具即完整 URL），你看到可疑内容可以直接拒绝；
- 「永久允许」写进 `providers.json`，可以随时在设置里清掉；
  **外发型工具不提供「永久允许」**，最多记住到本次会话结束；
- 一旦某个工具读过本地数据，这一轮的上下文再发给模型服务商之前，
  会**再**问一次；
- 联网工具走 SSRF 白名单：只允许公网 `http(s)`，`file:`、本机、内网、
  云元数据地址（`169.254.169.254`）在发请求前就被拒绝。

关于图片：

- 图片以 data URL 存在**本机 SQLite** 里（`messages.images_json`），
  删会话时随外键级联一起删掉，不会留下孤儿文件；
- 只接受 png / jpeg / webp / gif；`data:text/html` 与 `data:image/svg+xml`
  这类能执行脚本的形态**在落库前**就被拒绝（图片最终要进 WebView 的 `<img>`）；
- 图片只在**你发送的那一轮**随请求发给模型服务商，和你打的字同等对待。

---

## 致谢与来源说明

这个项目是**学习之后的独立实现**：先逐层拆解了参考项目 **ZHIMA-agent**
（芝麻桌面助手，Tauri 2 + Rust 桌面 AI 助手），再用 Rust 从零写出自己的版本。
架构取舍上有它的影子 —— 比如把 SSE 拆成「字节协议层 / 语义层」两段、
只在流开始之前重试、以及沿用凭据服务名 `ChatFloat`（改名等于让老用户的
API Key 全部失效，属于绝对不能动的量）。

需要说清楚的三件事：

1. **本仓库全部代码都是重写的**，不是参考项目的拷贝、翻译或分支。
   功能范围、数据结构、命名、错误处理都按自己的判断重新设计过，
   取舍与差异记在 `docs/01-架构设计.md`。
2. **参考项目没有附带开源许可证**（既无 `LICENSE` 文件，`package.json` /
   `Cargo.toml` 里也没有 `license` 字段，按版权规则即「保留所有权利」）。
   因此作者对它的逐层分析笔记（`docs/analysis/`，含约 326 KB 原样摘抄的
   代码片段）**只保留在作者本地，没有随本仓库发布** —— 把第三方代码
   放进一个以 MIT 发布的仓库既不合法，也会让 MIT 声明本身不成立。
   `docs/02-学习路线.md` 里保留了这四篇的**索引**（读了哪些模块、
   各多大体量），但文档内容不在仓库内。
3. 如果你想参考其中的设计，请以**参考项目作者自己的许可**为准。
   本仓库的 MIT 协议只覆盖作者自己写的代码与文档。

如果这个仓库里出现了应当标注来源却遗漏的地方，欢迎提 issue。
