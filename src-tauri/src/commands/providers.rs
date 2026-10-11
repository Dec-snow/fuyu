//! 服务商与模型的增删改查，以及 API Key 的存取。
//!
//! 两条硬规矩：
//! 1. **API Key 永远不出现在命令的返回值里** —— 前端能拿到的只有 `has_key: bool`；
//! 2. 这些命令全是 `async` —— Tauri 里非 async 的命令跑在**主线程**上，而这些
//!    命令要读系统凭据管理器（一次 OS 调用）和写配置文件（带 fsync 的落盘），
//!    放在主线程会直接卡住界面。

use serde::Serialize;
use tauri::State;

use crate::api::{openai, MAX_JSON_BODY};
use crate::errors::{read_body_capped, AppError, AppResult};
use crate::state::AppState;
use crate::storage::config::{AppConfig, ConfigStore, ModelEntry, ProviderConfig};
use crate::storage::secrets;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<ModelEntry>,
    pub is_preset: bool,
    /// 只暴露「有没有」，不暴露值本身。
    pub has_key: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvidersState {
    pub providers: Vec<ProviderView>,
    pub default_provider_id: Option<String>,
    pub default_model_key: Option<String>,
}

/// 组装给前端的状态。
///
/// 注意代价：`has_key` 需要对每个服务商各做一次系统凭据查询（Windows 上是
/// `CredRead`）。9 个预设大约是几次到几十毫秒，所以这个函数只在命令入口调用、
/// 并且整个命令是 `async` 的。再往下优化就是给凭据存在性加一层内存缓存，
/// 代价是要在 set/delete 时精确失效 —— 收益不大，暂时不做。
fn build_state(cfg: &AppConfig) -> ProvidersState {
    ProvidersState {
        providers: cfg
            .providers
            .iter()
            .map(|provider| ProviderView {
                id: provider.id.clone(),
                name: provider.name.clone(),
                base_url: provider.base_url.clone(),
                models: provider.models.clone(),
                is_preset: provider.is_preset,
                has_key: secrets::has_api_key(&provider.id),
            })
            .collect(),
        default_provider_id: cfg.default_provider_id.clone(),
        default_model_key: cfg.default_model_key.clone(),
    }
}

/// 读取服务商 / 模型 / 密钥存在性。
///
/// 返回 `AppResult` 而不是裸的 `ProvidersState`：**Tauri 的 `async` 命令必须
/// 返回 `Result`**（框架要靠它区分「成功值」和「错误值」两条 IPC 通道）。
/// 想返回裸值就只能写成同步命令，而那会把下面的凭据查询压回主线程。
#[tauri::command]
pub async fn get_providers_state(config: State<'_, ConfigStore>) -> AppResult<ProvidersState> {
    Ok(config.read(build_state))
}

/// 新增或更新一个服务商。
#[tauri::command]
pub async fn upsert_provider(
    config: State<'_, ConfigStore>,
    provider: ProviderConfig,
) -> AppResult<ProvidersState> {
    if provider.id.trim().is_empty() {
        return Err(AppError::invalid("服务商 ID 不能为空"));
    }
    // 顺手校验 Base URL，别等到发请求时才报错。
    let normalized = crate::storage::config::normalize_base_url(&provider.base_url)?;

    let state = config.update(|cfg| {
        let mut incoming = provider.clone();
        incoming.base_url = normalized.clone();
        match cfg.providers.iter_mut().find(|p| p.id == incoming.id) {
            Some(existing) => {
                // 预设的 id / 预设标记不允许被前端改掉。
                existing.name = incoming.name;
                existing.base_url = incoming.base_url;
                // 合并模型列表：保留已有的 favorite 标记。
                for model in incoming.models {
                    if !existing.has_model(&model.key) {
                        existing.models.push(model);
                    }
                }
            }
            None => cfg.providers.push(incoming),
        }
        Ok(())
    });
    state?;

    Ok(config.read(build_state))
}

#[tauri::command]
pub async fn delete_provider(
    config: State<'_, ConfigStore>,
    provider_id: String,
) -> AppResult<ProvidersState> {
    config.update(|cfg| {
        let Some(index) = cfg.providers.iter().position(|p| p.id == provider_id) else {
            return Err(AppError::invalid("该服务商不存在"));
        };
        if cfg.providers[index].is_preset {
            return Err(AppError::invalid("内置预设不能删除，只能修改 Base URL"));
        }
        cfg.providers.remove(index);
        // 删掉的正好是默认项 → 回退到第一个。
        if cfg.default_provider_id.as_deref() == Some(provider_id.as_str()) {
            cfg.default_provider_id = cfg.providers.first().map(|p| p.id.clone());
            cfg.default_model_key = cfg
                .providers
                .first()
                .and_then(|p| p.models.first())
                .map(|m| m.key.clone());
        }
        Ok(())
    })?;

    // 服务商没了，凭据也一起清掉，别在系统里留孤儿。
    if let Err(e) = secrets::delete_api_key(&provider_id) {
        eprintln!("清理服务商凭据失败：{e}");
    }

    // 必须在删完凭据之后才组装状态，否则 `has_key` 还是旧的。
    Ok(config.read(build_state))
}

/// 批量把模型加进某个服务商（「拉取模型列表」之后用）。
#[tauri::command]
pub async fn add_models(
    config: State<'_, ConfigStore>,
    provider_id: String,
    model_keys: Vec<String>,
) -> AppResult<ProvidersState> {
    config.update(|cfg| {
        let provider = cfg
            .providers
            .iter_mut()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| AppError::invalid("该服务商不存在"))?;
        for key in model_keys {
            let key = key.trim().to_string();
            if key.is_empty() || provider.has_model(&key) {
                continue;
            }
            provider.models.push(ModelEntry {
                key,
                favorite: false,
            });
        }
        Ok(())
    })?;

    Ok(config.read(build_state))
}

#[tauri::command]
pub async fn remove_model(
    config: State<'_, ConfigStore>,
    provider_id: String,
    model_key: String,
) -> AppResult<ProvidersState> {
    config.update(|cfg| {
        let is_default = cfg.default_provider_id.as_deref() == Some(provider_id.as_str())
            && cfg.default_model_key.as_deref() == Some(model_key.as_str());

        let provider = cfg
            .providers
            .iter_mut()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| AppError::invalid("该服务商不存在"))?;
        provider.models.retain(|m| m.key != model_key);

        if is_default {
            cfg.default_model_key = provider.models.first().map(|m| m.key.clone());
        }
        Ok(())
    })?;

    Ok(config.read(build_state))
}

#[tauri::command]
pub async fn set_default_model(
    config: State<'_, ConfigStore>,
    provider_id: String,
    model_key: String,
) -> AppResult<ProvidersState> {
    config.update(|cfg| {
        if cfg.provider(&provider_id).is_none() {
            return Err(AppError::invalid("该服务商不存在"));
        }
        cfg.default_provider_id = Some(provider_id);
        cfg.default_model_key = Some(model_key);
        Ok(())
    })?;

    Ok(config.read(build_state))
}

/// 写入（或清除）某个服务商的 API Key。
#[tauri::command]
pub async fn set_api_key(
    config: State<'_, ConfigStore>,
    provider_id: String,
    api_key: String,
) -> AppResult<ProvidersState> {
    if config.read(|cfg| cfg.provider(&provider_id).is_none()) {
        return Err(AppError::invalid("该服务商不存在"));
    }
    secrets::set_api_key(&provider_id, &api_key).map_err(AppError::secret)?;
    Ok(config.read(build_state))
}

/// 拉取服务商的模型列表（`GET /models`）。
///
/// 有些厂商不实现这个端点，所以它是「锦上添花」而非必走路径 —— 失败时前端
/// 应当提示用户可以手动输入模型名。
#[tauri::command]
pub async fn fetch_models(
    state: State<'_, AppState>,
    config: State<'_, ConfigStore>,
    provider_id: String,
) -> AppResult<Vec<String>> {
    let provider = config
        .read(|c| c.provider(&provider_id).cloned())
        .ok_or_else(|| AppError::invalid("该服务商不存在"))?;

    let api_key = secrets::get_api_key(&provider.id)
        .map_err(AppError::secret)?
        .unwrap_or_default();
    let url = openai::models_url(&provider.base_url)?;

    let mut request = state.client().get(&url);
    if !api_key.is_empty() {
        request = request.bearer_auth(&api_key);
    }
    let response = request.send().await?;
    let status = response.status();
    let body = read_body_capped(response, MAX_JSON_BODY).await?;

    if !status.is_success() {
        return Err(openai::map_http_error(status.as_u16(), &body));
    }
    openai::parse_models_response(&body)
}
