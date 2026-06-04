//! Workspace-inspired routes: model compare, deep research, cookbook-lite (Odysseus parity).

use crate::api_http::json_response;
use akasha_llm::config::TaskTypeConfig;
use akasha_llm::provider::CompletionRequest;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn parse_json_body(body: Option<&[u8]>) -> Option<serde_json::Value> {
    body.and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
}

/// Returns `Some(response)` when this module handled the route.
pub async fn handle_workspace_routes(
    method: &str,
    path_only: &str,
    body: Option<&[u8]>,
    llm_router: &Arc<akasha_llm::LLMRouter>,
) -> Option<String> {
    if method == "GET" && path_only == "/api/cookbook/hardware" {
        return Some(json_response(
            "200 OK",
            &hardware_snapshot_json().to_string(),
        ));
    }

    if method == "GET" && path_only == "/api/cookbook/recommendations" {
        let snap = hardware_snapshot_json();
        let ram = snap
            .get("total_ram_gb")
            .and_then(|v| v.as_u64())
            .unwrap_or(16);
        let gpu = snap
            .get("gpu_hint")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let client = crate::cookbook_models::http_client();
        let local_runtimes =
            crate::cookbook_models::local_runtime_status(&client, llm_router).await;
        let ollama_models: Vec<String> = local_runtimes
            .get("ollama")
            .and_then(|o| o.get("models"))
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();
        let rbitnet_models: Vec<String> = local_runtimes
            .get("rbitnet")
            .and_then(|o| o.get("models"))
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();
        let catalog =
            crate::cookbook_models::build_cookbook_catalog(llm_router).await;
        let providers_map = catalog.providers;
        let mut huggingface_local =
            crate::cookbook_models::fetch_huggingface_local_models(&client, ram, gpu).await;
        huggingface_local = huggingface_local
            .into_iter()
            .map(|e| crate::cookbook_models::apply_local_install(e, &ollama_models, &rbitnet_models))
            .collect();
        let routes = llm_router.routes_by_category();
        let (configured, suggestions) = cookbook_recommendations(
            &snap,
            &providers_map,
            &catalog.model_meta,
            &routes,
            &ollama_models,
            &rbitnet_models,
        );
        let mut task_categories: Vec<String> = routes.keys().cloned().collect();
        task_categories.sort();
        let mut registered_providers: Vec<String> = providers_map.keys().cloned().collect();
        registered_providers.sort();
        return Some(json_response(
            "200 OK",
            &serde_json::json!({
                "hardware": snap,
                "configured_providers": providers_map,
                "registered_providers": registered_providers,
                "task_categories": task_categories,
                "local_runtimes": local_runtimes,
                "recommendations": configured,
                "suggestions": suggestions,
                "huggingface_local": huggingface_local,
            })
            .to_string(),
        ));
    }

    if method == "POST" && path_only == "/api/cookbook/local/pull" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body"}"#,
            ));
        };
        let runtime = v.get("runtime").and_then(|x| x.as_str()).unwrap_or("").trim();
        let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("").trim();
        if model.is_empty() || (runtime != "ollama" && runtime != "rbitnet") {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"invalid_body","hint":"runtime=ollama|rbitnet and model required"}"#,
            ));
        }
        let result = if runtime == "ollama" {
            let base = crate::cookbook_models::resolve_ollama_base_url(llm_router);
            crate::cookbook_models::pull_ollama_model(&base, model)
                .await
                .map(|msg| serde_json::json!({ "ok": true, "runtime": "ollama", "message": msg }))
                .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e }))
        } else {
            crate::cookbook_models::install_rbitnet_model(model)
                .map(|msg| serde_json::json!({ "ok": true, "runtime": "rbitnet", "message": msg }))
                .unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e }))
        };
        let status = if result.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            "200 OK"
        } else {
            "502 Bad Gateway"
        };
        return Some(json_response(status, &result.to_string()));
    }

    if method == "POST" && path_only == "/api/compare" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body","hint":"JSON body required"}"#,
            ));
        };
        let prompt = v
            .get("prompt")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if prompt.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_prompt"}"#,
            ));
        }
        let blind = v.get("blind").and_then(|x| x.as_bool()).unwrap_or(false);
        let models = v.get("models").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if models.is_empty() || models.len() > 6 {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"models_required","hint":"1-6 entries with provider+model"}"#,
            ));
        }
        return Some(run_compare(llm_router, prompt, &models, blind).await);
    }

    if method == "POST" && path_only == "/api/research/deep" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body","hint":"JSON body required"}"#,
            ));
        };
        let topic = v
            .get("topic")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if topic.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_topic"}"#,
            ));
        }
        let max_steps = v
            .get("max_steps")
            .and_then(|x| x.as_u64())
            .unwrap_or(3)
            .clamp(1, 8) as u32;
        return Some(run_deep_research(llm_router, topic, max_steps).await);
    }

    if method == "POST" && path_only == "/api/research/deep/plan" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body"}"#,
            ));
        };
        let topic = v.get("topic").and_then(|x| x.as_str()).unwrap_or("").trim();
        if topic.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"missing_topic"}"#));
        }
        let max_steps = v
            .get("max_steps")
            .and_then(|x| x.as_u64())
            .unwrap_or(3)
            .clamp(1, 8) as u32;
        return Some(plan_deep_research(llm_router, topic, max_steps).await);
    }

    if method == "POST" && path_only == "/api/research/deep/step" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body"}"#,
            ));
        };
        let topic = v.get("topic").and_then(|x| x.as_str()).unwrap_or("").trim();
        let question = v.get("question").and_then(|x| x.as_str()).unwrap_or("").trim();
        if topic.is_empty() || question.is_empty() {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_topic_or_question"}"#,
            ));
        }
        let step = v.get("step").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
        let total_steps = v
            .get("total_steps")
            .and_then(|x| x.as_u64())
            .unwrap_or(1)
            .clamp(1, 8) as u32;
        return Some(
            run_research_step(llm_router, topic, question, step, total_steps).await,
        );
    }

    if method == "POST" && path_only == "/api/research/deep/synthesize" {
        let Some(v) = parse_json_body(body) else {
            return Some(json_response(
                "400 Bad Request",
                r#"{"error":"missing_body"}"#,
            ));
        };
        let topic = v.get("topic").and_then(|x| x.as_str()).unwrap_or("").trim();
        if topic.is_empty() {
            return Some(json_response("400 Bad Request", r#"{"error":"missing_topic"}"#));
        }
        let sections = v.get("sections").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        return Some(synthesize_research_report(llm_router, topic, &sections).await);
    }

    None
}

fn hardware_snapshot_json() -> serde_json::Value {
    let total_ram_gb = std::env::var("AKASHA_HOST_RAM_GB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(16);
    serde_json::json!({
        "total_ram_gb": total_ram_gb,
        "gpu_hint": crate::cookbook_models::resolve_gpu_hint(),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hint": "Set AKASHA_HOST_RAM_GB for RAM; AKASHA_COOKBOOK_GPU_HINT overrides GPU auto-detect"
    })
}

fn cookbook_recommendations(
    hw: &serde_json::Value,
    providers_map: &HashMap<String, Vec<String>>,
    provider_meta: &HashMap<String, HashMap<String, serde_json::Value>>,
    routes: &HashMap<String, TaskTypeConfig>,
    ollama_models: &[String],
    rbitnet_models: &[String],
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let ram = hw.get("total_ram_gb").and_then(|v| v.as_u64()).unwrap_or(8);
    let gpu = hw
        .get("gpu_hint")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let configured = crate::cookbook_models::configured_model_entries(
        providers_map,
        routes,
        ram,
        gpu,
        provider_meta,
        ollama_models,
        rbitnet_models,
    );
    let seen: HashSet<String> = configured
        .iter()
        .filter_map(|e| {
            let p = e.get("provider")?.as_str()?;
            let m = e.get("model")?.as_str()?;
            Some(format!("{p}/{m}"))
        })
        .collect();
    let suggestions = hardware_suggestions(hw, ram, gpu, provider_meta, &seen, ollama_models, rbitnet_models);
    (configured, suggestions)
}

fn hardware_suggestions(
    hw: &serde_json::Value,
    ram: u64,
    _gpu: &str,
    provider_meta: &HashMap<String, HashMap<String, serde_json::Value>>,
    already: &HashSet<String>,
    ollama_models: &[String],
    rbitnet_models: &[String],
) -> Vec<serde_json::Value> {
    let gpu = hw
        .get("gpu_hint")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let mut raw = Vec::new();
    if ram >= 32 {
        raw.push(serde_json::json!({
            "id": "rbitnet-7b",
            "label": "Rbitnet / BitNet ~7B (local)",
            "provider": "bitnet",
            "model": "default",
            "fit_score": 0.92,
            "source": "suggestion",
            "notes": "Configure bitnet.base_url in llm_router.yaml; see Rbitnet/docs/USAGE.md"
        }));
        raw.push(serde_json::json!({
            "id": "ollama-llama3",
            "label": "Ollama llama3.2 (3B)",
            "provider": "ollama",
            "model": "llama3.2",
            "fit_score": 0.85,
            "source": "suggestion",
            "notes": "Good balance on 32GB+ hosts"
        }));
    } else if ram >= 16 {
        raw.push(serde_json::json!({
            "id": "embedded",
            "label": "Akasha embedded Qwen3 0.6B",
            "provider": "akasha_embedded",
            "model": "default",
            "fit_score": 0.95,
            "source": "suggestion",
            "notes": "Zero-config; already bundled"
        }));
        raw.push(serde_json::json!({
            "id": "ollama-small",
            "label": "Ollama small model (≤3B)",
            "provider": "ollama",
            "model": "llama3.2:1b",
            "fit_score": 0.78,
            "source": "suggestion",
            "notes": "Install Ollama; akasha config models set conversation ollama <model>"
        }));
    } else {
        raw.push(serde_json::json!({
            "id": "embedded",
            "label": "Akasha embedded (recommended)",
            "provider": "akasha_embedded",
            "model": "default",
            "fit_score": 0.98,
            "source": "suggestion",
            "notes": "Best fit for ≤16GB RAM"
        }));
        raw.push(serde_json::json!({
            "id": "cloud-fallback",
            "label": "OpenRouter / OpenAI API",
            "provider": "openrouter",
            "model": "gpt-4o-mini",
            "fit_score": 0.70,
            "source": "suggestion",
            "notes": "Offload inference when local RAM is limited"
        }));
    }
    if gpu != "unknown" && gpu != "none" {
        raw.push(serde_json::json!({
            "id": "gpu-local",
            "label": "Local GPU inference (Ollama / BitNet)",
            "provider": "ollama",
            "model": "(see Ollama tags)",
            "fit_score": 0.86,
            "source": "suggestion",
            "notes": format!("GPU hint: {gpu} — prefer local providers when VRAM allows")
        }));
    }
    raw.into_iter()
        .filter(|e| {
            let p = e.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let m = e.get("model").and_then(|v| v.as_str()).unwrap_or("");
            !already.contains(&format!("{p}/{m}"))
        })
        .map(|e| {
            crate::cookbook_models::apply_local_install(
                crate::cookbook_models::attach_model_details(
                    crate::cookbook_models::enrich_cookbook_entry(e),
                    ram,
                    gpu,
                    provider_meta,
                ),
                ollama_models,
                rbitnet_models,
            )
        })
        .collect()
}

async fn run_compare(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    prompt: &str,
    models: &[serde_json::Value],
    blind: bool,
) -> String {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for (i, m) in models.iter().enumerate() {
        let provider = m.get("provider").and_then(|x| x.as_str()).unwrap_or("").trim();
        let model = m.get("model").and_then(|x| x.as_str()).unwrap_or("").trim();
        if provider.is_empty() || model.is_empty() {
            return json_response(
                "400 Bad Request",
                r#"{"error":"invalid_model_entry","hint":"each model needs provider and model"}"#,
            );
        }
        let label = if blind {
            format!("Model {}", (b'A' + (i as u8).min(25)) as char)
        } else {
            format!("{provider}/{model}")
        };
        entries.push((label, provider.to_string(), model.to_string()));
    }

    let mut results = Vec::new();
    for (label, provider, model) in &entries {
        llm_router.set_primary_route(
            "compare_slot",
            akasha_llm::config::RouteEntry {
                provider: provider.clone(),
                model: model.clone(),
                config: None,
            },
        );
        let req = CompletionRequest {
            prompt: prompt.to_string(),
            max_tokens: Some(1024),
            temperature: Some(0.7),
            preferred_task_type: Some("compare_slot".to_string()),
            system_prompt: Some(
                "Answer the user prompt directly. Be concise unless the question requires detail."
                    .to_string(),
            ),
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };
        let timeout = std::time::Duration::from_secs(120);
        let started = std::time::Instant::now();
        let outcome = match tokio::time::timeout(timeout, llm_router.complete(&req)).await {
            Ok(Ok(resp)) => {
                let latency_ms = resp
                    .total_duration_ns
                    .map(|ns| ns / 1_000_000)
                    .unwrap_or_else(|| started.elapsed().as_millis() as u64);
                let (prompt_tokens, completion_tokens) = resp
                    .usage
                    .as_ref()
                    .map(|u| (u.prompt_tokens, u.completion_tokens))
                    .unwrap_or((0, 0));
                serde_json::json!({
                "label": label,
                "provider": if blind { serde_json::Value::Null } else { serde_json::json!(provider) },
                "model": if blind { serde_json::Value::Null } else { serde_json::json!(model) },
                "text": resp.text,
                "latency_ms": latency_ms,
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "cost_usd": resp.cost_usd,
                "model_used": resp.model_used,
                "ok": true
            })
            }
            Ok(Err(e)) => serde_json::json!({
                "label": label,
                "error": e,
                "ok": false
            }),
            Err(_) => serde_json::json!({
                "label": label,
                "error": "timeout",
                "ok": false
            }),
        };
        results.push(outcome);
    }

    let synthesis_prompt = format!(
        "The user asked:\n{prompt}\n\nHere are {} model answers (labels may be blind):\n{}\n\nWrite a short synthesis: agreements, disagreements, and which label seems best and why.",
        results.len(),
        results
            .iter()
            .filter_map(|r| {
                let label = r.get("label")?.as_str()?;
                let text = r.get("text").and_then(|t| t.as_str()).unwrap_or("[error]");
                Some(format!("--- {label} ---\n{text}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let synth_req = CompletionRequest {
        prompt: synthesis_prompt,
        max_tokens: Some(800),
        temperature: Some(0.4),
        preferred_task_type: Some("utility".to_string()),
        system_prompt: Some("You compare model outputs fairly.".to_string()),
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let synthesis = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        llm_router.complete(&synth_req),
    )
    .await
    {
        Ok(Ok(r)) => r.text,
        _ => String::new(),
    };

    json_response(
        "200 OK",
        &serde_json::json!({
            "prompt": prompt,
            "blind": blind,
            "results": results,
            "synthesis": synthesis
        })
        .to_string(),
    )
}

async fn plan_deep_research(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    topic: &str,
    max_steps: u32,
) -> String {
    let sub_questions = research_plan_subquestions(llm_router, topic, max_steps).await;
    json_response(
        "200 OK",
        &serde_json::json!({
            "topic": topic,
            "max_steps": max_steps,
            "sub_questions": sub_questions
        })
        .to_string(),
    )
}

async fn research_plan_subquestions(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    topic: &str,
    max_steps: u32,
) -> Vec<String> {
    let plan_prompt = format!(
        "Research topic: {topic}\n\nProduce a JSON array of {max_steps} search sub-questions (strings only, no markdown). Example: [\"q1\",\"q2\"]"
    );
    let plan_req = CompletionRequest {
        prompt: plan_prompt,
        max_tokens: Some(512),
        temperature: Some(0.3),
        preferred_task_type: Some("research".to_string()),
        system_prompt: Some("Output valid JSON array only.".to_string()),
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let sub_questions: Vec<String> = match llm_router.complete(&plan_req).await {
        Ok(r) => {
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&r.text.trim()) {
                arr
            } else if let Some(start) = r.text.find('[') {
                if let Some(end) = r.text.rfind(']') {
                    serde_json::from_str(&r.text[start..=end]).unwrap_or_default()
                } else {
                    vec![topic.to_string()]
                }
            } else {
                vec![topic.to_string()]
            }
        }
        Err(_) => vec![topic.to_string()],
    };
    sub_questions
        .into_iter()
        .take(max_steps as usize)
        .collect()
}

async fn run_research_step(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    topic: &str,
    question: &str,
    step: u32,
    total_steps: u32,
) -> String {
    let step_prompt = format!(
        "Research topic: {topic}\n\nResearch step {step}/{total_steps} — answer this sub-question with structured notes and cite sources as [Source: title — url] when you infer them from general knowledge:\n\n{question}"
    );
    let req = CompletionRequest {
        prompt: step_prompt,
        max_tokens: Some(1200),
        temperature: Some(0.5),
        preferred_task_type: Some("research".to_string()),
        system_prompt: None,
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let body = match llm_router.complete(&req).await {
        Ok(r) => r.text,
        Err(e) => format!("(step failed: {e})"),
    };
    json_response(
        "200 OK",
        &serde_json::json!({
            "step": step,
            "question": question,
            "body": body
        })
        .to_string(),
    )
}

async fn synthesize_research_report(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    topic: &str,
    sections: &[serde_json::Value],
) -> String {
    let report_prompt = format!(
        "Write a markdown research report on: {topic}\n\nUse these sections:\n{}",
        sections
            .iter()
            .filter_map(|s| {
                let q = s.get("question")?.as_str()?;
                let b = s.get("body")?.as_str()?;
                Some(format!("## {q}\n{b}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let report_req = CompletionRequest {
        prompt: report_prompt,
        max_tokens: Some(2048),
        temperature: Some(0.4),
        preferred_task_type: Some("research".to_string()),
        system_prompt: Some(
            "Produce a clear markdown report with ## headings, bullet points, and a ## Sources section."
                .to_string(),
        ),
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let report_md = match llm_router.complete(&report_req).await {
        Ok(r) => r.text,
        Err(e) => format!("# Research report\n\n(report synthesis failed: {e})"),
    };
    json_response(
        "200 OK",
        &serde_json::json!({
            "topic": topic,
            "report_markdown": report_md
        })
        .to_string(),
    )
}

async fn run_deep_research(
    llm_router: &Arc<akasha_llm::LLMRouter>,
    topic: &str,
    max_steps: u32,
) -> String {
    let sub_questions = research_plan_subquestions(llm_router, topic, max_steps).await;

    let mut sections = Vec::new();
    for (i, q) in sub_questions.iter().enumerate() {
        let step = i as u32 + 1;
        let step_prompt = format!(
            "Research step {step}/{max_steps} — answer this sub-question with structured notes and cite sources as [Source: title — url] when you infer them from general knowledge:\n\n{q}"
        );
        let req = CompletionRequest {
            prompt: step_prompt,
            max_tokens: Some(1200),
            temperature: Some(0.5),
            preferred_task_type: Some("research".to_string()),
            system_prompt: None,
            image_data_urls: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
        };
        let body = match llm_router.complete(&req).await {
            Ok(r) => r.text,
            Err(e) => format!("(step failed: {e})"),
        };
        sections.push(serde_json::json!({
            "step": step,
            "question": q,
            "body": body
        }));
    }

    let report_prompt = format!(
        "Write a markdown research report on: {topic}\n\nUse these sections:\n{}",
        sections
            .iter()
            .filter_map(|s| {
                let q = s.get("question")?.as_str()?;
                let b = s.get("body")?.as_str()?;
                Some(format!("## {q}\n{b}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let report_req = CompletionRequest {
        prompt: report_prompt,
        max_tokens: Some(2048),
        temperature: Some(0.4),
        preferred_task_type: Some("research".to_string()),
        system_prompt: Some(
            "Produce a clear markdown report with ## headings, bullet points, and a ## Sources section."
                .to_string(),
        ),
        image_data_urls: None,
        top_p: None,
        top_k: None,
        frequency_penalty: None,
        presence_penalty: None,
        repeat_penalty: None,
        num_ctx: None,
        num_gpu: None,
        thinking_level: None,
    };
    let report_md = match llm_router.complete(&report_req).await {
        Ok(r) => r.text,
        Err(e) => format!("# Research report\n\n(report synthesis failed: {e})"),
    };

    json_response(
        "200 OK",
        &serde_json::json!({
            "topic": topic,
            "max_steps": max_steps,
            "sections": sections,
            "report_markdown": report_md
        })
        .to_string(),
    )
}
