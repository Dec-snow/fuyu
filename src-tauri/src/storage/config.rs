//! 配置文件（`providers.json`）：服务商、模型、界面与行为偏好。
//!
//! 刻意**不存 API Key** —— 密钥走系统凭据管理器（见 [`super::secrets`]）。
//! 这样即使把 `providers.json` 发给别人，也不会泄密。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

/// 窗口背景不透明度的下限。
///
/// 为什么必须有下限：这个值最终会被前端写进 CSS 变量，而 0 的后果是
/// **整个窗口全透明** —— 用户看不见界面，自然也点不到设置改回来，
/// 只能手改 `providers.json`。一个滑块能让应用变得无法使用，这是设计缺陷。
pub const MIN_WINDOW_OPACITY: f32 = 0.35;

/// 正文字号范围（px）。
///
/// 下限 11px：再小在 125% 缩放的屏幕上读起来很吃力；
/// 上限 18px：再大一条消息就占满整个悬浮窗，反而不好用。
pub const MIN_FONT_SIZE: f32 = 11.0;
pub const MAX_FONT_SIZE: f32 = 18.0;
/// 默认正文号，和 CSS 里 `body` 原本的字号一致（改设置不该顺手改变默认外观）。
pub const DEFAULT_FONT_SIZE: f32 = 13.5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub key: String,
    #[serde(default)]
    pub favorite: bool,
}

/// 一个人设（角色预设）。
///
/// 为什么不直接把提示词抄进会话的 `system_prompt` 字段：抄一份的话，
/// 用户后来改了角色的措辞，已有会话不会跟着变（看起来像「改了没用」）；
/// 而且界面没法显示「这个会话现在是哪个角色」，只能拿提示词去反查 ——
/// 改一个标点就匹配不上了。所以会话存的是 `role_id`（见 v13 迁移）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub id: String,
    pub name: String,
    pub prompt: String,
    /// 内置模板：可以改内容，但不能删（删了就找不回来了）。
    #[serde(default)]
    pub builtin: bool,
}

impl Role {
    /// 内置人设模板。
    ///
    /// 为什么要有内置的：绝大多数用户并不知道自己该写什么系统提示词，
    /// 「翻译 / 代码 / 文案 / 总结」这四个覆盖了日常大部分场景，挑一个就能用。
    /// 用户改的是**自己的副本**，模板本身保持在预设里。
    pub fn presets() -> Vec<Role> {
        vec![
            Role {
                id: "builtin-translator".to_string(),
                name: "翻译官".to_string(),
                builtin: true,
                prompt: "你是一名专业翻译。把用户发来的内容翻译成自然、地道的目标语言；\
                         未指定目标语言时按中英互译处理。只输出译文，不要解释、不要加引号，\
                         保留原文的段落结构与代码块。"
                    .to_string(),
            },
            Role {
                id: "builtin-coder".to_string(),
                name: "代码助手".to_string(),
                builtin: true,
                prompt: "你是一名严谨的工程师。回答以「能直接用」为准：先给结论或完整代码，\
                         再简短说明关键点。代码要能运行并处理边界情况；\
                         不确定的地方明确说出来，不要编造 API 或参数。"
                    .to_string(),
            },
            Role {
                id: "builtin-writer".to_string(),
                name: "文案写手".to_string(),
                builtin: true,
                prompt: "你是一名中文文案写手。文字简洁、有节奏，避免空话套话和堆砌形容词。\
                         需要多个版本时并列给出，并说明各自适合的场景。"
                    .to_string(),
            },
            Role {
                id: "builtin-summarizer".to_string(),
                name: "严谨总结".to_string(),
                builtin: true,
                prompt: "你负责把材料压缩成要点。只写材料里确实存在的信息，\
                         不要补充外部知识；区分「材料明确说的」和「你的推断」。\
                         用短句与列表，保留关键数字与结论。"
                    .to_string(),
            },
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    /// 形如 `https://api.deepseek.com/v1`（不带 `/chat/completions`）。
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// 是否来自内置预设（预设不可删除，只能改 Base URL / 填 Key）。
    #[serde(default)]
    pub is_preset: bool,
}

impl ProviderConfig {
    pub fn has_model(&self, key: &str) -> bool {
        self.models.iter().any(|m| m.key == key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub providers: Vec<ProviderConfig>,
    pub default_provider_id: Option<String>,
    pub default_model_key: Option<String>,
    /// 全局默认系统提示词，可在设置里改。
    pub system_prompt: String,
    /// `system` / `light` / `dark`
    pub theme: String,
    /// 唤醒悬浮窗的全局热键。
    pub shortcut: String,
    /// 是否启用划词助手（Alt+Q 取词）。
    pub quick_action_enabled: bool,
    /// 划词助手热键。
    pub quick_action_shortcut: String,
    /// 失焦自动隐藏。
    pub hide_on_blur: bool,
    pub remember_window_position: bool,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    /// 完整模式的窗口尺寸（悬浮模式固定为顶部 560×268）。
    pub expanded_width: u32,
    pub expanded_height: u32,
    /// 上次退出时是悬浮模式还是完整模式。
    pub expanded: bool,
    pub proxy_url: Option<String>,
    pub use_system_proxy: bool,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    /// 发请求时的上下文预算（token）。超出就从最旧的消息开始丢。
    pub context_budget_tokens: u32,
    /// 用户选择「永久允许」的工具名称列表（三级授权里的 always 一档）。
    #[serde(default)]
    pub always_allowed_tools: Vec<String>,
    /// SearXNG 兼容的联网搜索端点（留空 = 不启用联网搜索，工具不下发给模型）。
    #[serde(default)]
    pub web_search_endpoint: Option<String>,
    /// 悬浮窗**背景**的不透明度（[`MIN_WINDOW_OPACITY`] ~ 1.0）。
    ///
    /// 只影响背景，不影响文字：调低之后文字仍然完全不透明，
    /// 否则「调透明」会变成「调得看不清」。
    #[serde(default = "default_window_opacity")]
    pub window_opacity: f32,
    /// 正文字号（[`MIN_FONT_SIZE`] ~ [`MAX_FONT_SIZE`]，px）。
    #[serde(default = "default_font_size")]
    pub message_font_size: f32,
    /// 角色预设（内置 + 用户自建）。
    #[serde(default = "Role::presets")]
    pub roles: Vec<Role>,
    /// 开机自启。
    ///
    /// 这里存的是**用户的选择**；系统里的真实状态可能被任务管理器改掉，
    /// 所以界面上显示的是 `autostart_status` 命令读回来的值（见
    /// `commands/autostart.rs`）。
    #[serde(default)]
    pub autostart: bool,
    /// 长期记忆。
    ///
    /// **默认关闭**：打开之后，每次提问都会把相关的记忆一并发给模型服务商 ——
    /// 那本质上就是「把本地数据发出去」。这个项目既有的立场是「工具默认关闭、
    /// 读过本地敏感数据要再确认一次」，所以这里也默认关，由用户明确打开。
    #[serde(default)]
    pub memory_enabled: bool,
    /// 一问多答的对比目标（最多 4 个）。
    ///
    /// 存的是「上次对比用的是哪几个模型」—— 这是一个频繁重复的选择，
    /// 每次打开面板都重选一遍很烦；但它不是「当前生效的配置」，
    /// 面板里随时可以改。
    #[serde(default)]
    pub compare_targets: Vec<CompareTarget>,
}

/// 一问多答里的一个目标（服务商 + 模型）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareTarget {
    pub provider_id: String,
    pub model_key: String,
}

fn default_window_opacity() -> f32 {
    1.0
}

fn default_font_size() -> f32 {
    DEFAULT_FONT_SIZE
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            default_provider_id: None,
            default_model_key: None,
            system_prompt: "你是一个简洁、准确的桌面助手。回答尽量直接，必要时才展开。".to_string(),
            theme: "system".to_string(),
            shortcut: "Alt+Space".to_string(),
            quick_action_enabled: true,
            quick_action_shortcut: "Alt+Q".to_string(),
            hide_on_blur: true,
            remember_window_position: false,
            window_x: None,
            window_y: None,
            expanded_width: 900,
            expanded_height: 680,
            expanded: false,
            proxy_url: None,
            // 默认跟随系统/环境变量里的代理设置 —— 也就是 reqwest 的默认行为。
            // 默认 `false` 会让「什么都没配」的用户在无感知的情况下**失去**
            // 环境变量代理，那不是我们该替用户做的决定。
            use_system_proxy: true,
            temperature: 0.7,
            max_tokens: None,
            context_budget_tokens: crate::agent::context::DEFAULT_CONTEXT_BUDGET_TOKENS as u32,
            always_allowed_tools: Vec::new(),
            web_search_endpoint: None,
            window_opacity: default_window_opacity(),
            message_font_size: default_font_size(),
            roles: Role::presets(),
            autostart: false,
            memory_enabled: false,
            compare_targets: Vec::new(),
        }
    }
}

impl AppConfig {
    /// 首次运行时用内置预设铺一份服务商列表，并把第一个设为默认。
    pub fn seeded() -> Self {
        let providers: Vec<ProviderConfig> = crate::models::config::PRESETS
            .iter()
            .map(|p| ProviderConfig {
                id: p.id.to_string(),
                name: p.name.to_string(),
                base_url: p.base_url.to_string(),
                models: p
                    .models
                    .iter()
                    .map(|m| ModelEntry {
                        key: (*m).to_string(),
                        favorite: false,
                    })
                    .collect(),
                is_preset: true,
            })
            .collect();

        let default_provider_id = providers.first().map(|p| p.id.clone());
        let default_model_key = providers
            .first()
            .and_then(|p| p.models.first())
            .map(|m| m.key.clone());

        Self {
            providers,
            default_provider_id,
            default_model_key,
            ..Self::default()
        }
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }
}

/// 线程安全的配置句柄。
///
/// `Clone` 只是克隆两个 `Arc`，可以放心塞进 `AppState`、也可以 move 进
/// 异步任务里（Tauri 的 `State<'_, T>` 不能跨 `.await` 持有）。
#[derive(Clone)]
pub struct ConfigStore {
    path: Arc<PathBuf>,
    inner: Arc<RwLock<AppConfig>>,
}

impl ConfigStore {
    /// 读取磁盘配置；文件不存在或损坏时回退到预设并写回一份。
    pub fn load(path: PathBuf) -> Self {
        let cfg = match std::fs::read_to_string(&path) {
            Ok(text) => {
                // 记事本 / PowerShell 保存 UTF-8 时常带 BOM，`serde_json`
                // 不接受它。先剥掉，免得一次无关的编辑就把配置判成损坏。
                let text = text.trim_start_matches('\u{feff}');
                match serde_json::from_str::<AppConfig>(text) {
                    Ok(cfg) => cfg,
                    Err(e) => recover_from_unusable(&path, &format!("内容不是合法 JSON：{e}")),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 文件**真的不存在** = 首次启动，写一份预设出去。
                let seeded = AppConfig::seeded();
                if let Err(e) = persist(&path, &seeded) {
                    eprintln!("初始化配置文件失败：{e}");
                }
                seeded
            }
            Err(e) => {
                // 文件**存在**却读不出文本：编码不是 UTF-8（记事本存成 ANSI /
                // UTF-16LE，PowerShell 5.1 的 `>` 默认也是 UTF-16）、权限不足、
                // 或磁盘出错。
                //
                // 这里绝不能当成「首次启动」直接写默认值 —— 用户的文件好端端
                // 躺在那里（可能手改过，可能只是编码不对，改回来就能用），
                // 一写就永久没了。走和「解析失败」同一条留档路径。
                recover_from_unusable(&path, &format!("无法读取：{e}"))
            }
        };
        Self {
            path: Arc::new(path),
            inner: Arc::new(RwLock::new(cfg)),
        }
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// 只读访问。闭包内不要做 IO，否则会长时间占着读锁。
    pub fn read<T>(&self, f: impl FnOnce(&AppConfig) -> T) -> T {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// 读改写 + 落盘。返回闭包的返回值。
    ///
    /// **两条失败路径都要回滚内存**：
    /// - 闭包自己返回 `Err`（多字段补丁常见：改了 `theme` 才在 `temperature`
    ///   上报错）；
    /// - 落盘失败。
    ///
    /// 只回滚后者的话，会出现「内存里是新值、磁盘上是旧值」—— 界面显示已生效，
    /// 重启就变回去，而且下一次任何成功的写入会把这个「被拒绝的值」悄悄落盘。
    pub fn update<T>(&self, f: impl FnOnce(&mut AppConfig) -> AppResult<T>) -> AppResult<T> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let snapshot = guard.clone();
        let out = match f(&mut guard) {
            Ok(out) => out,
            Err(e) => {
                *guard = snapshot;
                return Err(e);
            }
        };
        if let Err(e) = persist(&self.path, &guard) {
            *guard = snapshot;
            return Err(e);
        }
        Ok(out)
    }
}

/// 配置文件**存在但不可用**时的兜底：先留档，再决定要不要写默认值。
///
/// 留档失败时**什么都不写**：宁可这一次只在内存里用默认值跑，也不能把一个
/// 还可能救回来的文件覆盖掉。用户此时去备份还来得及。
fn recover_from_unusable(path: &Path, reason: &str) -> AppConfig {
    let seeded = AppConfig::seeded();
    match quarantine(path) {
        Some(backup) => {
            eprintln!(
                "配置文件不可用（{reason}），原文件已留档到 {}，本次以默认值启动",
                backup.display()
            );
            if let Err(e) = persist(path, &seeded) {
                eprintln!("写入默认配置失败：{e}");
            }
        }
        None => {
            eprintln!(
                "配置文件不可用（{reason}），且**无法改名留档**：\
                 为避免覆盖掉还可能救回的内容，本次只在内存里使用默认值，\
                 不改动磁盘上的原文件。"
            );
        }
    }
    seeded
}

/// 把不可用的配置改名留档，返回留档路径；改名失败返回 `None`。
///
/// **必须把失败告诉调用方**：否则调用方会以为「已经留档了」而放心去覆盖原文件 ——
/// 在 Windows 上这很现实：文件被编辑器 / 杀毒 / 备份工具以「允许读写但不允许删除」
/// 的方式占着时，`rename` 会失败，而 `File::create` 照样能把它清空。
fn quarantine(path: &Path) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let candidate = path.with_extension(format!("corrupt-{stamp}.json"));
    std::fs::rename(path, &candidate).ok().map(|()| candidate)
}

/// 原子写入：先写临时文件并 `fsync`，再 `rename` 覆盖目标。
///
/// 直接覆写原文件的话，写到一半断电就会得到一个半截 JSON —— 配置全丢。
/// `rename` 在同一分区上是原子的，所以要么是旧文件、要么是新文件。
fn persist(path: &Path, cfg: &AppConfig) -> AppResult<()> {
    use std::io::Write;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(cfg)?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 校验并规范化用户填的 Base URL。
pub fn normalize_base_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::invalid("Base URL 不能为空"));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(AppError::invalid(
            "Base URL 必须以 http:// 或 https:// 开头",
        ));
    }
    // 用户经常把完整的 chat/completions 一起粘进来，这里容错。
    let trimmed = trimmed
        .trim_end_matches("/chat/completions")
        .trim_end_matches('/');
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_utf8_config_is_quarantined_not_treated_as_first_run() {
        let (dir, path) = temp_config_path();

        // 记事本「ANSI」/「Unicode」以及 PowerShell 5.1 的 `>` 默认都会写出
        // 非 UTF-8 的内容；这里用 UTF-16LE 复现。`read_to_string` 会报 InvalidData，
        // 而**不是** NotFound —— 旧实现把它当成「首次启动」，直接写默认值把
        // 用户手改过的配置永久覆盖掉。
        let utf16: Vec<u8> = "{\"theme\":\"dark\"}"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        std::fs::write(&path, &utf16).unwrap();

        let store = ConfigStore::load(path.clone());

        // 应用要能起来（退回预设）。
        assert!(!store.read(|c| c.providers.is_empty()));

        // 关键：原文件必须被**留档**。
        let quarantined: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("providers.corrupt-"))
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "非 UTF-8 的配置也必须留档：{quarantined:?}"
        );

        let kept = std::fs::read(dir.join(&quarantined[0])).unwrap();
        assert_eq!(kept, utf16, "留档的必须是原始字节，不能是转码后的");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_rolls_back_when_the_closure_itself_fails() {
        let (dir, path) = temp_config_path();
        let store = ConfigStore::load(path.clone());
        let before = store.read(|c| c.theme.clone());

        // 模拟多字段补丁：改了 theme 才在后面的字段上报错。
        let result: AppResult<()> = store.update(|cfg| {
            cfg.theme = "dark".to_string();
            Err(AppError::invalid("temperature 超范围"))
        });
        assert!(result.is_err());

        // 内存必须回滚 —— 否则界面显示已生效、重启又变回去，
        // 而且下一次任何成功的写入会把这个「被拒绝的值」悄悄落盘。
        assert_eq!(
            store.read(|c| c.theme.clone()),
            before,
            "闭包失败也要回滚内存"
        );

        let reloaded = ConfigStore::load(path);
        assert_eq!(reloaded.read(|c| c.theme.clone()), before, "磁盘上也不该变");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 配置的 JSON 键名必须和 `src/types.ts` 里的保持一致。
    ///
    /// 这是**跨语言的隐式契约**：`AppConfig` 用 `rename_all = "camelCase"`，
    /// 前端按 camelCase 读。哪边改错了，`tsc` 也不会报错 —— 只会表现为
    /// 「这个设置项怎么改都没反应」。所以在这里把名字钉住。
    #[test]
    fn the_config_json_uses_the_camel_case_keys_the_frontend_reads() {
        let json = serde_json::to_value(AppConfig::seeded()).unwrap();
        for key in [
            "alwaysAllowedTools",
            "useSystemProxy",
            "proxyUrl",
            "contextBudgetTokens",
            "systemPrompt",
            "quickActionShortcut",
            "webSearchEndpoint",
            "maxTokens",
            "hideOnBlur",
            "rememberWindowPosition",
        ] {
            assert!(json.get(key).is_some(), "配置 JSON 里缺少字段 {key}");
        }

        // 反向也要成立：写回去能读出来（旧配置文件缺字段时用默认值）。
        let text = serde_json::to_string(&AppConfig::seeded()).unwrap();
        let parsed: AppConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed.always_allowed_tools,
            AppConfig::seeded().always_allowed_tools
        );
        assert_eq!(
            parsed.use_system_proxy,
            AppConfig::seeded().use_system_proxy
        );
    }

    #[test]
    fn seeded_config_has_a_default_provider() {
        let cfg = AppConfig::seeded();
        assert!(!cfg.providers.is_empty());
        assert!(cfg.default_provider_id.is_some());
        assert!(cfg.default_model_key.is_some());
    }

    #[test]
    fn base_url_is_normalized() {
        assert_eq!(
            normalize_base_url(" https://api.deepseek.com/v1/ ").unwrap(),
            "https://api.deepseek.com/v1"
        );
        // 完整端点也能被纠正回 base。
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1/chat/completions").unwrap(),
            "https://api.deepseek.com/v1"
        );
        assert!(normalize_base_url("api.deepseek.com").is_err());
        assert!(normalize_base_url("").is_err());
    }

    #[test]
    fn config_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("floatai-test-{}", crate::storage::new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.json");

        let store = ConfigStore::load(path.clone());
        store
            .update(|cfg| {
                cfg.theme = "dark".to_string();
                cfg.temperature = 0.3;
                Ok(())
            })
            .unwrap();

        let reloaded = ConfigStore::load(path);
        assert_eq!(reloaded.read(|c| c.theme.clone()), "dark");
        assert!((reloaded.read(|c| c.temperature) - 0.3).abs() < f32::EPSILON);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_config_path() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("floatai-cfg-{}", crate::storage::new_id("")));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("providers.json");
        (dir, path)
    }

    #[test]
    fn a_config_written_by_an_older_version_still_loads() {
        // 这是最容易踩的一类坑：新版本加了一个配置字段，老用户机器上那份
        // `providers.json` 里没有它。容器上带了 `#[serde(default)]`，所以缺的
        // 字段会回落到 `AppConfig::default()` —— 但这条约定一旦被谁删掉，
        // 表现是「升级后配置整个读不出来、所有服务商都消失」，而且不报错。
        let (dir, path) = temp_config_path();
        std::fs::write(
            &path,
            r#"{"theme":"dark","temperature":0.3,"providers":[
                 {"id":"custom","name":"我的中转","baseUrl":"https://example.com/v1","models":[]}
               ]}"#,
        )
        .unwrap();

        let store = ConfigStore::load(path);

        // 文件里有的字段照常生效。
        assert_eq!(store.read(|c| c.theme.clone()), "dark");
        assert!((store.read(|c| c.temperature) - 0.3).abs() < f32::EPSILON);
        assert_eq!(store.read(|c| c.providers.len()), 1);
        // 没有的字段回落到默认值，而不是让整份配置解析失败。
        assert_eq!(
            store.read(|c| c.context_budget_tokens),
            crate::agent::context::DEFAULT_CONTEXT_BUDGET_TOKENS as u32
        );
        assert_eq!(store.read(|c| c.shortcut.clone()), "Alt+Space");
        assert!(store.read(|c| c.hide_on_blur));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_config_falls_back_to_presets_instead_of_panicking() {
        let (dir, path) = temp_config_path();
        std::fs::write(&path, "{ 这不是 JSON").unwrap();

        let store = ConfigStore::load(path.clone());

        // 坏文件不能让应用起不来：退回预设，至少还能用。
        assert!(!store.read(|c| c.providers.is_empty()));
        assert_eq!(
            store.read(|c| c.providers.len()),
            crate::models::config::PRESETS.len()
        );

        // 坏文件必须被**留档**，而不是被默认值直接覆盖 —— 里面可能有用户
        // 加过的服务商，静默抹掉是不可接受的。
        let quarantined: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("providers.corrupt-"))
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "应当留档一份损坏配置，实际：{quarantined:?}"
        );
        let kept = std::fs::read_to_string(dir.join(&quarantined[0])).unwrap();
        assert_eq!(kept, "{ 这不是 JSON", "留档的必须是原样内容");

        // 磁盘上现在是一份可用的默认配置（重启也不会再触发留档）。
        let reloaded = ConfigStore::load(path);
        assert_eq!(
            reloaded.read(|c| c.providers.len()),
            crate::models::config::PRESETS.len()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_utf8_bom_does_not_count_as_corruption() {
        let (dir, path) = temp_config_path();

        // 记事本保存 UTF-8 会带 BOM；这不该让整份配置被判成损坏。
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"theme":"dark","shortcut":"Alt+Space"}"#);
        std::fs::write(&path, &bytes).unwrap();

        let store = ConfigStore::load(path.clone());

        assert_eq!(store.read(|c| c.theme.clone()), "dark", "BOM 不该影响解析");
        // 没有产生留档文件 = 没被当成损坏。
        let quarantined = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt"))
            .count();
        assert_eq!(quarantined, 0, "带 BOM 的合法配置不该被留档");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_update_rolls_back_the_in_memory_value() {
        // 把配置文件路径换成一个「父级是文件」的路径，落盘必然失败。
        let (dir, path) = temp_config_path();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();

        let store = ConfigStore::load(path);
        let before = store.read(|c| c.theme.clone());

        // `blocker` 是个文件，把它当目录用 → 落盘必然失败。
        let doomed = ConfigStore::load(blocker.join("providers.json"));
        let result = doomed.update(|cfg| {
            cfg.theme = "dark".to_string();
            Ok(())
        });

        assert!(result.is_err(), "落盘失败必须报错");
        assert_eq!(
            doomed.read(|c| c.theme.clone()),
            before,
            "落盘失败后内存值必须回滚，否则界面显示的和磁盘上的不一致"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
