use crate::config::{Config, ModelConfig};

/// Calls a model from `[[models]]` for a hook. Returns the response text.
pub async fn complete(cfg: &Config, name: &str, system: Option<&str>, prompt: &str) -> anyhow::Result<String> {
    let model = cfg
        .models
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("no such model: {name}"))?;
    let api_key = match &model.api_key_env {
        Some(var) => Some(std::env::var(var).map_err(|_| anyhow::anyhow!("model {name}: {var} is not set"))?),
        None => None,
    };
    let client = reqwest::Client::new();

    match model.api.as_str() {
        "anthropic" => complete_anthropic(&client, model, api_key.as_deref(), system, prompt).await,
        "openai" => complete_openai_responses(&client, model, api_key.as_deref(), system, prompt).await,
        "openai_chat" => complete_openai_chat(&client, model, api_key.as_deref(), system, prompt).await,
        other => anyhow::bail!("unknown model api: {other}"),
    }
}

async fn complete_anthropic(
    client: &reqwest::Client,
    model: &ModelConfig,
    api_key: Option<&str>,
    system: Option<&str>,
    prompt: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model.model,
        "max_tokens": 2048,
        "messages": [{"role": "user", "content": prompt}],
    });
    if let Some(system) = system {
        body["system"] = serde_json::json!(system);
    }

    let mut req = client.post(&url).header("anthropic-version", "2023-06-01").json(&body);
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("anthropic model {name}: {status}: {text}", name = model.name);
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let out = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    Ok(out)
}

async fn complete_openai_responses(
    client: &reqwest::Client,
    model: &ModelConfig,
    api_key: Option<&str>,
    system: Option<&str>,
    prompt: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/v1/responses", model.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": model.model,
        "input": prompt,
    });
    if let Some(system) = system {
        body["instructions"] = serde_json::json!(system);
    }

    let mut req = client.post(&url).json(&body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("openai model {name}: {status}: {text}", name = model.name);
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(extract_responses_text(&v))
}

fn extract_responses_text(v: &serde_json::Value) -> String {
    if let Some(s) = v["output_text"].as_str() {
        return s.to_string();
    }
    v["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

async fn complete_openai_chat(
    client: &reqwest::Client,
    model: &ModelConfig,
    api_key: Option<&str>,
    system: Option<&str>,
    prompt: &str,
) -> anyhow::Result<String> {
    let url = format!("{}/v1/chat/completions", model.base_url.trim_end_matches('/'));
    let mut messages = Vec::new();
    if let Some(system) = system {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    messages.push(serde_json::json!({"role": "user", "content": prompt}));
    let body = serde_json::json!({
        "model": model.model,
        "messages": messages,
    });

    let mut req = client.post(&url).json(&body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("openai_chat model {name}: {status}: {text}", name = model.name);
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}
