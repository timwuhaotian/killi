use super::*;
use anyhow::bail;
use crate::config::NewModel;

/// The models the plan offers, in the server's order.
pub(super) async fn plan_models(http: &reqwest::Client, token: &str) -> Result<Vec<NewModel>> {
    let response = http.get(format!("{RESOURCE}/models")).header("authorization", format!("Bearer {token}")).send().await?;
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", crate::llm::truncate(&String::from_utf8_lossy(&body), 300));
    }
    Ok(models_from(&serde_json::from_slice(&body)?))
}

/// Reads the plan's catalog: `models` entries with `slug` and `visibility`, and when given,
/// `context_window`, `supported_reasoning_levels`, `default_reasoning_level` and
/// `input_modalities`. A plain OpenAI list (`data` with `id`s) is read too.
pub(super) fn models_from(catalog: &Value) -> Vec<NewModel> {
    let listed = catalog["models"].as_array().into_iter().flatten().filter(|model| model["visibility"] == "list");
    let mut models: Vec<NewModel> = listed
        .filter_map(|model| {
            Some(NewModel {
                id: model["slug"].as_str().filter(|slug| !slug.is_empty())?.to_string(),
                context: model["context_window"].as_u64(),
                efforts: model["supported_reasoning_levels"].as_array().into_iter().flatten().filter_map(|level| level["effort"].as_str().map(String::from)).collect(),
                default_effort: model["default_reasoning_level"].as_str().map(String::from),
                images: model["input_modalities"].as_array().is_some_and(|inputs| inputs.iter().any(|input| input == "image")),
                thinking: true,
                tools: true,
                ..NewModel::default()
            })
        })
        .collect();
    if models.is_empty() {
        let ids = catalog["data"].as_array().into_iter().flatten().filter_map(|model| model["id"].as_str());
        models = ids.map(|id| NewModel { id: id.to_string(), thinking: true, tools: true, ..NewModel::default() }).collect();
    }
    models
}
