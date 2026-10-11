//! 暴露给前端的应用信息与默认服务商预设。

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub name: String,
    pub db_path: String,
    pub config_path: String,
}

/// 一键预设的常用服务商（都是 Chat Completions 兼容接口）。
///
/// 只提供 Base URL 与模型清单，**不含任何密钥**；密钥由用户自己填。
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        models: &["deepseek-chat", "deepseek-reasoner"],
    },
    ProviderPreset {
        id: "zhipu",
        name: "智谱 AI",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &["glm-4-plus", "glm-4-flash", "glm-4v-plus"],
    },
    ProviderPreset {
        id: "moonshot",
        name: "Kimi (Moonshot)",
        base_url: "https://api.moonshot.cn/v1",
        models: &["moonshot-v1-8k", "moonshot-v1-32k", "kimi-latest"],
    },
    ProviderPreset {
        id: "dashscope",
        name: "通义千问",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        models: &["qwen-plus", "qwen-max", "qwen-turbo"],
    },
    ProviderPreset {
        id: "minimax",
        name: "MiniMax",
        base_url: "https://api.minimax.chat/v1",
        models: &["abab6.5s-chat"],
    },
    ProviderPreset {
        id: "siliconflow",
        name: "硅基流动",
        base_url: "https://api.siliconflow.cn/v1",
        models: &["deepseek-ai/DeepSeek-V3", "Qwen/Qwen2.5-7B-Instruct"],
    },
    ProviderPreset {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        models: &["gpt-4o-mini", "gpt-4o"],
    },
    ProviderPreset {
        id: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        models: &["openai/gpt-4o-mini"],
    },
    ProviderPreset {
        id: "ollama",
        name: "本地 Ollama",
        base_url: "http://localhost:11434/v1",
        models: &["qwen2.5:7b", "llama3.1:8b"],
    },
];
