//! 应用信息与全局设置。

use serde::Deserialize;
use tauri::State;

use crate::errors::{AppError, AppResult};
use crate::models::config::AppInfo;
use crate::storage::config::{
    AppConfig, CompareTarget, ConfigStore, ProviderConfig, Role, Skill, MAX_FONT_SIZE,
    MIN_FONT_SIZE, MIN_WINDOW_OPACITY,
};
use crate::storage::database::Database;

#[tauri::command]
pub fn app_info(db: State<'_, Database>, config: State<'_, ConfigStore>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        name: "浮语 FloatAI".to_string(),
        db_path: db.path().display().to_string(),
        config_path: config.path().display().to_string(),
    }
}

#[tauri::command]
pub fn get_config(config: State<'_, ConfigStore>) -> AppConfig {
    config.read(|c| c.clone())
}

/// 应用设置的增量补丁。
///
/// 用「只带要改的字段」而不是整份配置覆盖，避免前端某个过期副本把用户
/// 刚在别处改的设置又冲掉。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    pub theme: Option<String>,
    pub system_prompt: Option<String>,
    pub shortcut: Option<String>,
    pub hide_on_blur: Option<bool>,
    pub remember_window_position: Option<bool>,
    /// 划词助手开关（Alt+Q）。
    pub quick_action_enabled: Option<bool>,
    /// SearXNG 搜索端点（空串 = 关闭联网搜索）。
    pub web_search_endpoint: Option<String>,
    /// 空字符串表示「不使用代理」。
    pub proxy_url: Option<String>,
    pub use_system_proxy: Option<bool>,
    pub temperature: Option<f32>,
    /// 0 表示「不限制」。
    pub max_tokens: Option<u32>,
    pub default_provider_id: Option<String>,
    pub default_model_key: Option<String>,
    /// 上下文预算（token）。超出就从最旧的消息开始丢。
    pub context_budget_tokens: Option<u32>,
    /// 「永久允许」过哪些工具。传整个列表（不是增量）——
    /// 界面上的操作是「撤销某一项」，整表覆写比算差集简单也更难出错。
    pub always_allowed_tools: Option<Vec<String>>,
    /// 悬浮窗背景不透明度（会被收进 `MIN_WINDOW_OPACITY ..= 1.0`）。
    pub window_opacity: Option<f32>,
    /// 正文字号 px（会被收进 `MIN_FONT_SIZE ..= MAX_FONT_SIZE`）。
    pub message_font_size: Option<f32>,
    /// 角色预设。整表覆写 —— 与 lways_allowed_tools 同一个思路：
    /// 界面上的操作是「改一个 / 删一个」，整表覆写比算差集简单也更难出错。
    pub roles: Option<Vec<Role>>,
    /// 长期记忆开关（打开后会把相关记忆随请求发给服务商）。
    pub memory_enabled: Option<bool>,
    /// 一问多答的对比目标（整表覆写，与 roles 同一个思路）。
    pub compare_targets: Option<Vec<CompareTarget>>,
    /// 技能列表（整表覆写）。
    pub skills: Option<Vec<Skill>>,
}

/// 角色数量上限。
///
/// 不是洁癖：角色提示词会被拼进每一次请求，一份塞了几百个角色的配置会让
/// 每个请求白白多花几千 token，设置面板也会长到没法用。
/// 一问多答最多几个模型。与前端 lib/compare.ts 的 MAX_COMPARE_TARGETS 对齐。
const MAX_COMPARE_TARGETS: usize = 4;

/// 技能数量上限。
const MAX_SKILLS: usize = 50;

const MAX_ROLES: usize = 50;
/// 单个角色提示词的长度上限（字符数）。
const MAX_ROLE_PROMPT_CHARS: usize = 4_000;

/// 规整技能列表。
///
/// 与 `sanitize_roles` 的差别只有一处：技能带工具白名单，而白名单里的名字必须是
/// **真的存在的工具**。写错一个名字的后果很隐蔽 —— 技能看起来正常，但那个工具
/// 永远不生效（清单被过滤掉了），用户只会觉得「开了也没用」。所以这里**报错**
/// 而不是静默丢掉，并且在消息里说清是哪个名字不认识。
fn sanitize_skills(skills: Vec<Skill>) -> AppResult<Vec<Skill>> {
    if skills.len() > MAX_SKILLS {
        return Err(AppError::invalid(format!("技能最多 {MAX_SKILLS} 个")));
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(skills.len());
    for mut skill in skills {
        skill.id = skill.id.trim().to_string();
        skill.name = skill.name.trim().to_string();
        skill.prompt = skill.prompt.trim().to_string();
        skill.description = skill.description.trim().to_string();
        skill.tools = skill
            .tools
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();

        if skill.id.is_empty() {
            return Err(AppError::invalid("技能 id 不能为空"));
        }
        if skill.name.is_empty() {
            return Err(AppError::invalid("技能名字不能为空"));
        }
        if skill.prompt.is_empty() {
            return Err(AppError::invalid(format!(
                "技能「{}」的指令不能为空",
                skill.name
            )));
        }
        if !seen.insert(skill.id.clone()) {
            return Err(AppError::invalid(format!("技能 id 重复：{}", skill.id)));
        }
        for tool in &skill.tools {
            if crate::tools::registry::find(tool).is_none() {
                return Err(AppError::invalid(format!(
                    "技能「{}」里有一个不存在的工具名：{tool}",
                    skill.name
                )));
            }
        }
        out.push(skill);
    }
    Ok(out)
}

/// 规整对比目标：去空、去重、丢掉已经不存在的服务商、夹到上限。
///
/// 与 `sanitize_roles` 不同，这里**静默丢弃**坏项而不是报错：对比目标是一个
/// 「上次用过哪几个模型」的偏好，用户在设置里删掉一个服务商、下次打开面板时
/// 那个目标自然该消失 —— 为此弹一个错误反而莫名其妙。
fn sanitize_compare_targets(
    targets: Vec<CompareTarget>,
    providers: &[ProviderConfig],
) -> Vec<CompareTarget> {
    let known: std::collections::HashSet<&str> = providers.iter().map(|p| p.id.as_str()).collect();

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for target in targets {
        let provider_id = target.provider_id.trim().to_string();
        let model_key = target.model_key.trim().to_string();
        if provider_id.is_empty() || model_key.is_empty() {
            continue;
        }
        if !known.contains(provider_id.as_str()) {
            continue;
        }
        if !seen.insert((provider_id.clone(), model_key.clone())) {
            continue;
        }
        out.push(CompareTarget {
            provider_id,
            model_key,
        });
        if out.len() >= MAX_COMPARE_TARGETS {
            break;
        }
    }
    out
}

/// 规整角色列表：名字与提示词不能为空、id 不能重复、数量与长度都有上限。
///
/// 坏数据返回 `Err` 而不是静默丢弃：这些值会被原样发给模型，
/// 悄悄改掉用户写的东西比报错更让人困惑。
fn sanitize_roles(roles: Vec<Role>) -> AppResult<Vec<Role>> {
    if roles.len() > MAX_ROLES {
        return Err(AppError::invalid(format!("角色最多 {MAX_ROLES} 个")));
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(roles.len());
    for mut role in roles {
        role.id = role.id.trim().to_string();
        role.name = role.name.trim().to_string();
        role.prompt = role.prompt.trim().to_string();

        if role.id.is_empty() {
            return Err(AppError::invalid("角色 id 不能为空"));
        }
        if role.name.is_empty() {
            return Err(AppError::invalid("角色名字不能为空"));
        }
        if role.prompt.is_empty() {
            return Err(AppError::invalid(format!(
                "角色「{}」的提示词不能为空",
                role.name
            )));
        }
        if role.prompt.chars().count() > MAX_ROLE_PROMPT_CHARS {
            return Err(AppError::invalid(format!(
                "角色「{}」的提示词太长（上限 {MAX_ROLE_PROMPT_CHARS} 字）",
                role.name
            )));
        }
        if !seen.insert(role.id.clone()) {
            return Err(AppError::invalid(format!("角色 id 重复：{}", role.id)));
        }
        out.push(role);
    }
    Ok(out)
}

/// 把用户给的不透明度收进合法区间。
///
/// 抽成纯函数是为了能单测 —— 命令本身要 `State<'_, ConfigStore>`，构造不出来，
/// 而这段「非法值怎么处理」恰恰是最该被钉死的：`NaN` 会让 CSS 变量失效，
/// `0` 会让窗口彻底看不见。
fn clamp_opacity(v: f32) -> AppResult<f32> {
    if !v.is_finite() {
        return Err(AppError::invalid("窗口不透明度必须是有效数字"));
    }
    Ok(v.clamp(MIN_WINDOW_OPACITY, 1.0))
}

/// 字号同理：非有限值必须报错，其余收进区间。
fn clamp_font_size(v: f32) -> AppResult<f32> {
    if !v.is_finite() {
        return Err(AppError::invalid("字号必须是有效数字"));
    }
    Ok(v.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE))
}

/// 规整「永久允许」的工具名单：只收**已知**工具名，去重并排序。
///
/// 只收已知名字这一点是安全相关的，不是洁癖：这条白名单是敏感工具唯一的
/// 守门人，如果放任意字符串进来，一份被篡改的配置就能预先「永久允许」
/// 一个以后版本才会出现的工具 —— 那等于给未来的自己留了一个后门。
///
/// 用 `web_search_enabled = true` 取全集：搜索工具是否可用取决于用户有没有配
/// 端点，但**能不能永久允许**不该跟着变，否则关掉搜索再打开就会丢掉授权。
fn sanitize_always_allowed(tools: Vec<String>) -> AppResult<Vec<String>> {
    // 名字现在是 owned `String`（MCP 工具是运行时发现的），所以这里也用 String 收集
    let known: Vec<String> =
        crate::tools::registry::tool_definitions(crate::tools::registry::ToolAvailability::all())
            .into_iter()
            .map(|d| d.name)
            .collect();

    let mut next = Vec::with_capacity(tools.len());
    for name in tools {
        if !known.contains(&name) {
            return Err(AppError::invalid(format!("未知的工具名：{name}")));
        }
        next.push(name);
    }
    next.sort();
    next.dedup();
    Ok(next)
}

/// `async`：`ConfigStore::update` 会做一次带 `fsync` 的原子落盘，
/// 同步执行的话这段 IO 会占着主线程。
#[tauri::command]
pub async fn update_settings(
    config: State<'_, ConfigStore>,
    patch: SettingsPatch,
) -> AppResult<AppConfig> {
    config.update(|cfg| {
        if let Some(theme) = patch.theme {
            if !matches!(theme.as_str(), "system" | "light" | "dark") {
                return Err(AppError::invalid(format!("未知主题：{theme}")));
            }
            cfg.theme = theme;
        }
        if let Some(prompt) = patch.system_prompt {
            cfg.system_prompt = prompt;
        }
        if let Some(shortcut) = patch.shortcut {
            if shortcut.trim().is_empty() {
                return Err(AppError::invalid("快捷键不能为空"));
            }
            cfg.shortcut = shortcut;
        }
        if let Some(v) = patch.hide_on_blur {
            cfg.hide_on_blur = v;
        }
        if let Some(v) = patch.remember_window_position {
            cfg.remember_window_position = v;
        }
        if let Some(v) = patch.quick_action_enabled {
            cfg.quick_action_enabled = v;
        }
        if let Some(endpoint) = patch.web_search_endpoint {
            let trimmed = endpoint.trim().to_string();
            // 空串表示「关掉联网搜索」：`None` 会让 web_search 工具不下发给模型。
            cfg.web_search_endpoint = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            };
        }
        if let Some(url) = patch.proxy_url {
            let url = url.trim().to_string();
            cfg.proxy_url = if url.is_empty() { None } else { Some(url) };
        }
        if let Some(v) = patch.use_system_proxy {
            cfg.use_system_proxy = v;
        }
        if let Some(t) = patch.temperature {
            if !(0.0..=2.0).contains(&t) {
                return Err(AppError::invalid("temperature 必须在 0 到 2 之间"));
            }
            cfg.temperature = t;
        }
        if let Some(m) = patch.max_tokens {
            cfg.max_tokens = if m == 0 { None } else { Some(m) };
        }
        if let Some(id) = patch.default_provider_id {
            if cfg.provider(&id).is_none() {
                return Err(AppError::invalid("该服务商不存在"));
            }
            cfg.default_provider_id = Some(id);
        }
        if let Some(key) = patch.default_model_key {
            cfg.default_model_key = Some(key);
        }
        if let Some(budget) = patch.context_budget_tokens {
            // 1000 是「至少还能塞进一两轮对话」的下限；上限只是防手滑输错。
            cfg.context_budget_tokens = budget.clamp(1_000, 1_000_000);
        }
        if let Some(tools) = patch.always_allowed_tools {
            cfg.always_allowed_tools = sanitize_always_allowed(tools)?;
        }
        if let Some(v) = patch.window_opacity {
            cfg.window_opacity = clamp_opacity(v)?;
        }
        if let Some(v) = patch.message_font_size {
            cfg.message_font_size = clamp_font_size(v)?;
        }
        if let Some(roles) = patch.roles {
            cfg.roles = sanitize_roles(roles)?;
        }
        if let Some(enabled) = patch.memory_enabled {
            cfg.memory_enabled = enabled;
        }
        if let Some(targets) = patch.compare_targets {
            cfg.compare_targets = sanitize_compare_targets(targets, &cfg.providers);
        }
        if let Some(skills) = patch.skills {
            cfg.skills = sanitize_skills(skills)?;
        }
        // 改完代理立刻重建 HTTP 客户端，否则要重启才生效。
        Ok(cfg.clone())
    })
}

/// 改完代理设置后重建 HTTP 客户端。
#[tauri::command]
pub fn apply_proxy(
    state: State<'_, crate::state::AppState>,
    config: State<'_, ConfigStore>,
) -> AppResult<()> {
    let (proxy, use_system_proxy) = config.read(|c| (c.proxy_url.clone(), c.use_system_proxy));
    state.rebuild_http_client(proxy.as_deref(), use_system_proxy);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 窗口不透明度 / 字号：这一类「滑块型」设置必须被硬边界挡住
    //
    // 它们最终都会变成 CSS 变量。「不透明度 = 0」的后果不是「不好看」，而是
    // **窗口彻底看不见**：用户找不到设置面板，只能手改配置文件才能恢复。

    #[test]
    fn zero_opacity_is_clamped_up_to_the_floor() {
        assert_eq!(clamp_opacity(0.0).unwrap(), MIN_WINDOW_OPACITY);
        assert_eq!(clamp_opacity(-3.0).unwrap(), MIN_WINDOW_OPACITY);
    }

    #[test]
    fn opacity_above_one_is_clamped_down() {
        assert_eq!(clamp_opacity(1.8).unwrap(), 1.0);
        assert_eq!(clamp_opacity(1.0).unwrap(), 1.0);
    }

    #[test]
    fn a_sane_opacity_passes_through_unchanged() {
        assert_eq!(clamp_opacity(0.8).unwrap(), 0.8);
        assert_eq!(
            clamp_opacity(MIN_WINDOW_OPACITY).unwrap(),
            MIN_WINDOW_OPACITY
        );
    }

    #[test]
    fn a_non_finite_opacity_is_rejected_instead_of_silently_becoming_nan() {
        // NaN 写进 CSS 变量会让整条声明失效（背景退回浏览器默认），
        // 而且从界面上完全看不出发生了什么 —— 必须报错。
        assert!(clamp_opacity(f32::NAN).is_err());
        assert!(clamp_opacity(f32::INFINITY).is_err());
        assert!(clamp_font_size(f32::NAN).is_err());
    }

    #[test]
    fn font_size_is_clamped_into_range() {
        assert_eq!(clamp_font_size(2.0).unwrap(), MIN_FONT_SIZE);
        assert_eq!(clamp_font_size(99.0).unwrap(), MAX_FONT_SIZE);
        assert_eq!(clamp_font_size(15.0).unwrap(), 15.0);
    }

    // ---- 角色预设：这些值会被原样拼进每次请求，所以坏数据必须挡在写盘之前

    fn role(id: &str, name: &str, prompt: &str) -> Role {
        Role {
            id: id.to_string(),
            name: name.to_string(),
            prompt: prompt.to_string(),
            builtin: false,
        }
    }

    // ---------------- 技能

    fn skill(id: &str, name: &str, prompt: &str) -> Skill {
        Skill {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            prompt: prompt.to_string(),
            tools: Vec::new(),
            builtin: false,
        }
    }

    #[test]
    fn a_sane_skill_list_passes_and_gets_trimmed() {
        let mut s = skill(" s1 ", "  翻译 ", "  把内容翻成英文  ");
        s.tools = vec![" calculator ".to_string(), " ".to_string()];
        let out = sanitize_skills(vec![s]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "s1");
        assert_eq!(out[0].name, "翻译");
        assert_eq!(out[0].prompt, "把内容翻成英文");
        // 工具名去空格、空项丢掉
        assert_eq!(out[0].tools, vec!["calculator".to_string()]);
    }

    #[test]
    fn a_skill_without_name_or_prompt_is_rejected() {
        assert!(sanitize_skills(vec![skill("s1", "  ", "有指令")])
            .unwrap_err()
            .message
            .contains("名字"));
        assert!(sanitize_skills(vec![skill("s1", "名字", "  ")])
            .unwrap_err()
            .message
            .contains("指令"));
        // 重复 id 会让「按 id 找技能」变得不确定
        let dup = vec![skill("same", "A", "x"), skill("same", "B", "y")];
        assert!(sanitize_skills(dup).unwrap_err().message.contains("重复"));
    }

    #[test]
    fn an_unknown_tool_name_in_a_skill_is_rejected_loudly() {
        // 静默丢掉一个拼错的工具名，后果很隐蔽：技能看起来正常，但那个工具
        // 永远不生效（清单被过滤掉了），用户只会觉得「开了也没用」。
        let mut s = skill("s1", "调研", "先搜索再回答");
        s.tools = vec!["web_seach".to_string()]; // 故意拼错
        let err = sanitize_skills(vec![s]).unwrap_err();
        assert!(err.message.contains("web_seach"), "{}", err.message);
        assert!(
            err.message.contains("调研"),
            "要说清是哪个技能：{}",
            err.message
        );
    }

    #[test]
    fn too_many_skills_are_rejected() {
        let many = (0..=MAX_SKILLS)
            .map(|i| skill(&format!("s{i}"), &format!("技能{i}"), "指令"))
            .collect();
        assert!(sanitize_skills(many).unwrap_err().message.contains("最多"));
    }

    #[test]
    fn the_builtin_skill_presets_themselves_are_valid() {
        // 预设本身必须能过自己的校验 —— 否则「恢复默认」会直接报错
        let presets = Skill::presets();
        let out = sanitize_skills(presets.clone()).unwrap();
        assert_eq!(out.len(), presets.len());
        for preset in &presets {
            for tool in &preset.tools {
                assert!(
                    crate::tools::registry::find(tool).is_some(),
                    "内置技能「{}」引用了不存在的工具 {tool}",
                    preset.name
                );
            }
        }
    }

    #[test]
    fn only_the_intended_skills_restrict_tools() {
        // 「联网调研」与「纯离线总结」靠白名单做事，另外两个不该干预工具
        let presets = Skill::presets();
        let research = presets.iter().find(|s| s.id == "builtin-research").unwrap();
        assert!(research.restricts_tools() && research.allows("web_search"));
        let offline = presets
            .iter()
            .find(|s| s.id == "builtin-offline-summary")
            .unwrap();
        assert!(offline.restricts_tools());
        assert!(
            !offline.allows("web_search") && !offline.allows("fetch_webpage"),
            "「纯离线」绝不能给联网工具"
        );
        let translate = presets
            .iter()
            .find(|s| s.id == "builtin-translate")
            .unwrap();
        assert!(!translate.restricts_tools(), "翻译不该干预工具选择");
    }

    #[test]
    fn a_sane_role_list_passes_and_gets_trimmed() {
        let out = sanitize_roles(vec![role(" r1 ", "  翻译官 ", "  你是翻译。  ")]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "r1");
        assert_eq!(out[0].name, "翻译官");
        assert_eq!(out[0].prompt, "你是翻译。");
    }

    // ---------------- 一问多答的对比目标

    fn provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            base_url: String::new(),
            models: Vec::new(),
            is_preset: false,
        }
    }

    fn target(provider_id: &str, model_key: &str) -> CompareTarget {
        CompareTarget {
            provider_id: provider_id.to_string(),
            model_key: model_key.to_string(),
        }
    }

    #[test]
    fn compare_targets_are_trimmed_deduplicated_and_capped() {
        let providers = vec![provider("p1"), provider("p2")];
        let out = sanitize_compare_targets(
            vec![
                target(" p1 ", " m1 "),
                // 同一个「服务商 + 模型」重复：对比两次毫无意义，而消耗是实打实的
                target("p1", "m1"),
                target("p2", "m1"),
                target("p1", "m2"),
                target("p1", "m3"),
                target("p1", "m4"),
            ],
            &providers,
        );
        assert_eq!(out.len(), MAX_COMPARE_TARGETS, "应当夹到上限");
        assert_eq!(out[0].provider_id, "p1");
        assert_eq!(out[0].model_key, "m1", "首尾空格要去掉");
        // 去重后顺序保持不变
        assert_eq!(
            out.iter()
                .map(|t| format!("{}/{}", t.provider_id, t.model_key))
                .collect::<Vec<_>>(),
            vec!["p1/m1", "p2/m1", "p1/m2", "p1/m3"]
        );
    }

    #[test]
    fn compare_targets_drop_unknown_providers_and_blanks() {
        let providers = vec![provider("p1")];
        let out = sanitize_compare_targets(
            vec![
                target("", "m"),
                target("p1", "  "),
                // 服务商已经被删掉了：这是个「上次用过哪几个模型」的偏好，
                // 静默丢掉才对，为此弹错误反而莫名其妙
                target("已删除的服务商", "m"),
                target("p1", "m"),
            ],
            &providers,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].model_key, "m");
    }

    #[test]
    fn blank_name_or_prompt_is_rejected() {
        assert!(sanitize_roles(vec![role("r1", "   ", "prompt")]).is_err());
        assert!(sanitize_roles(vec![role("r1", "名字", "   ")]).is_err());
        assert!(sanitize_roles(vec![role("  ", "名字", "prompt")]).is_err());
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        // 重复 id 会让「会话绑定哪个角色」变得取决于遍历顺序 ——
        // 同一份配置在不同机器上可能解析出不同的角色。
        let err =
            sanitize_roles(vec![role("dup", "甲", "p1"), role("dup", "乙", "p2")]).unwrap_err();
        assert!(err.message.contains("重复"));
    }

    #[test]
    fn too_many_roles_are_rejected() {
        let many: Vec<Role> = (0..(MAX_ROLES + 1))
            .map(|i| role(&format!("r{i}"), "n", "p"))
            .collect();
        assert!(sanitize_roles(many).is_err());
        let ok: Vec<Role> = (0..MAX_ROLES)
            .map(|i| role(&format!("r{i}"), "n", "p"))
            .collect();
        assert!(sanitize_roles(ok).is_ok());
    }

    #[test]
    fn an_overlong_prompt_is_rejected() {
        let long = "啊".repeat(MAX_ROLE_PROMPT_CHARS + 1);
        let err = sanitize_roles(vec![role("r1", "长", &long)]).unwrap_err();
        assert!(err.message.contains("太长"));
        // 边界值本身要能通过（不能差一位）
        let at_limit = "啊".repeat(MAX_ROLE_PROMPT_CHARS);
        assert!(sanitize_roles(vec![role("r1", "长", &at_limit)]).is_ok());
    }

    #[test]
    fn the_builtin_presets_themselves_are_valid() {
        // 内置模板必须能通过自己的校验，否则「重置设置」会把配置写成非法状态。
        let presets = Role::presets();
        assert!(!presets.is_empty());
        let out = sanitize_roles(presets).unwrap();
        assert!(out.iter().all(|r| r.builtin));
        assert!(out.iter().all(|r| !r.prompt.is_empty()));
    }

    #[test]
    fn a_known_tool_can_be_remembered_forever() {
        let ok = sanitize_always_allowed(vec!["get_current_time".to_string()]).unwrap();
        assert_eq!(ok, vec!["get_current_time".to_string()]);
    }

    #[test]
    fn an_unknown_tool_name_is_rejected() {
        // 这条白名单是敏感工具唯一的守门人：放任意字符串进来，
        // 就等于允许「预先授权一个以后才会出现的工具」。
        let err = sanitize_always_allowed(vec!["rm_rf_everything".to_string()]).unwrap_err();
        assert_eq!(err.code, crate::errors::code::INVALID_INPUT);
        assert!(err.message.contains("rm_rf_everything"));
    }

    #[test]
    fn the_list_is_deduped_and_sorted() {
        let out = sanitize_always_allowed(vec![
            "write_clipboard_text".to_string(),
            "calculator".to_string(),
            "calculator".to_string(),
        ])
        .unwrap();

        // 界面上的顺序要稳定，配置文件也不该因为顺序抖动被反复改写。
        assert_eq!(out, vec!["calculator", "write_clipboard_text"]);
    }

    #[test]
    fn an_empty_list_clears_every_grant() {
        assert_eq!(
            sanitize_always_allowed(vec![]).unwrap(),
            Vec::<String>::new()
        );
    }
}
