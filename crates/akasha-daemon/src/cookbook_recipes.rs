//! Cookbook recipes catalog — static JSON embedded from spec/cookbook/.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipeLocale {
    pub title: String,
    pub summary: String,
    pub steps_md: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct RecipePrerequisites {
    #[serde(default)]
    pub task_types: Vec<String>,
    #[serde(default)]
    pub models_hint: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub min_ram_gb: Option<f64>,
    #[serde(default)]
    pub gpu_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipeVariable {
    pub key: String,
    pub label_en: String,
    pub label_fr: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub placeholder_en: Option<String>,
    #[serde(default)]
    pub placeholder_fr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipeConfigSnippet {
    pub language: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipeCommand {
    pub label_en: String,
    pub label_fr: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipeAction {
    #[serde(rename = "type")]
    pub action_type: String,
    pub label_en: String,
    pub label_fr: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecipeExternalLink {
    pub label_en: String,
    pub label_fr: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CookbookRecipe {
    pub id: String,
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub difficulty: String,
    pub last_reviewed: String,
    pub locales: HashMap<String, RecipeLocale>,
    #[serde(default)]
    pub prerequisites: RecipePrerequisites,
    #[serde(default)]
    pub prompt_template: Option<String>,
    #[serde(default)]
    pub variables: Vec<RecipeVariable>,
    #[serde(default)]
    pub config_snippet: Option<RecipeConfigSnippet>,
    #[serde(default)]
    pub commands: Vec<RecipeCommand>,
    #[serde(default)]
    pub actions: Vec<RecipeAction>,
    #[serde(default)]
    pub related_recipe_ids: Vec<String>,
    #[serde(default)]
    pub external_links: Vec<RecipeExternalLink>,
}

const RECIPE_BLOBS: &[(&str, &str)] = &[
    (
        "structured-json-output",
        include_str!("../../../spec/cookbook/recipes/prompts/structured-json-output.json"),
    ),
    (
        "agent-tool-loop",
        include_str!("../../../spec/cookbook/recipes/agents/agent-tool-loop.json"),
    ),
    (
        "utility-prompt-compaction",
        include_str!("../../../spec/cookbook/recipes/prompts/utility-prompt-compaction.json"),
    ),
    (
        "user-rag-setup",
        include_str!("../../../spec/cookbook/recipes/rag/user-rag-setup.json"),
    ),
    (
        "deep-research-workflow",
        include_str!("../../../spec/cookbook/recipes/rag/deep-research-workflow.json"),
    ),
    (
        "memory-ltm-rag",
        include_str!("../../../spec/cookbook/recipes/rag/memory-ltm-rag.json"),
    ),
    (
        "lora-dataset-prep",
        include_str!("../../../spec/cookbook/recipes/fine_tuning/lora-dataset-prep.json"),
    ),
    (
        "qlora-unsloth-local",
        include_str!("../../../spec/cookbook/recipes/fine_tuning/qlora-unsloth-local.json"),
    ),
    (
        "finetune-vs-rag-decision",
        include_str!("../../../spec/cookbook/recipes/fine_tuning/finetune-vs-rag-decision.json"),
    ),
    (
        "compare-blind-test",
        include_str!("../../../spec/cookbook/recipes/evaluation/compare-blind-test.json"),
    ),
    (
        "synthetic-dataset-hf",
        include_str!("../../../spec/cookbook/recipes/ml/synthetic-dataset-hf.json"),
    ),
    (
        "export-ollama-gguf",
        include_str!("../../../spec/cookbook/recipes/deployment/export-ollama-gguf.json"),
    ),
];

static REGISTRY: OnceLock<HashMap<String, CookbookRecipe>> = OnceLock::new();

fn registry() -> &'static HashMap<String, CookbookRecipe> {
    REGISTRY.get_or_init(|| {
        let mut map = HashMap::new();
        for (_id, blob) in RECIPE_BLOBS {
            match serde_json::from_str::<CookbookRecipe>(blob) {
                Ok(recipe) => {
                    map.insert(recipe.id.clone(), recipe);
                }
                Err(e) => {
                    eprintln!("cookbook recipe parse error: {e}");
                }
            }
        }
        map
    })
}

pub fn all_recipes() -> Vec<CookbookRecipe> {
    let mut out: Vec<CookbookRecipe> = registry().values().cloned().collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn get_recipe(id: &str) -> Option<CookbookRecipe> {
    registry().get(id).cloned()
}

pub fn recipe_summary(recipe: &CookbookRecipe, locale: &str) -> Value {
    let loc = recipe.locales.get(locale).or_else(|| recipe.locales.get("en"));
    serde_json::json!({
        "id": recipe.id,
        "category": recipe.category,
        "tags": recipe.tags,
        "difficulty": recipe.difficulty,
        "last_reviewed": recipe.last_reviewed,
        "title": loc.map(|l| l.title.as_str()).unwrap_or(""),
        "summary": loc.map(|l| l.summary.as_str()).unwrap_or(""),
    })
}

pub fn filter_recipes(
    category: Option<&str>,
    tag: Option<&str>,
    search: Option<&str>,
    locale: &str,
) -> Vec<Value> {
    let search_lc = search.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    all_recipes()
        .into_iter()
        .filter(|r| {
            if let Some(cat) = category {
                if !cat.is_empty() && cat != "all" && r.category != cat {
                    return false;
                }
            }
            if let Some(t) = tag {
                if !t.is_empty() && t != "all" && !r.tags.iter().any(|x| x == t) {
                    return false;
                }
            }
            if let Some(ref q) = search_lc {
                let loc = r.locales.get(locale).or_else(|| r.locales.get("en"));
                let hay = format!(
                    "{} {} {} {}",
                    r.id,
                    loc.map(|l| l.title.as_str()).unwrap_or(""),
                    loc.map(|l| l.summary.as_str()).unwrap_or(""),
                    r.tags.join(" ")
                )
                .to_lowercase();
                if !hay.contains(q.as_str()) {
                    return false;
                }
            }
            true
        })
        .map(|r| recipe_summary(&r, locale))
        .collect()
}

/// Score how well a cookbook model entry matches recipe prerequisites (0..1).
pub fn model_recipe_fit(entry: &Value, prereq: &RecipePrerequisites, host_ram_gb: u64) -> f64 {
    let mut score = entry
        .get("fit_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5);

    if let Some(min_ram) = prereq.min_ram_gb {
        if (host_ram_gb as f64) < min_ram {
            score *= 0.55;
        }
    }

    if !prereq.models_hint.is_empty() {
        let model = entry
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let types: Vec<String> = entry
            .get("model_types")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();
        let details = entry.get("details").and_then(|v| v.as_object());
        let agentic = details
            .and_then(|d| d.get("agentic"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut hint_match = false;
        for hint in &prereq.models_hint {
            let h = hint.to_lowercase();
            if model.contains(&h) || types.iter().any(|t| t.contains(&h)) {
                hint_match = true;
                break;
            }
            if h == "agentic" && agentic {
                hint_match = true;
                break;
            }
            if h == "instruct"
                && (model.contains("instruct") || types.contains(&"instruct".to_string()))
            {
                hint_match = true;
                break;
            }
        }
        if !hint_match {
            score *= 0.75;
        }
    }

    score.clamp(0.0, 1.0)
}

pub fn recommend_models_for_recipe(
    recipe: &CookbookRecipe,
    catalog_items: &[Value],
    host_ram_gb: u64,
    limit: usize,
) -> Vec<Value> {
    let mut scored: Vec<(f64, Value)> = catalog_items
        .iter()
        .cloned()
        .map(|e| {
            let fit = model_recipe_fit(&e, &recipe.prerequisites, host_ram_gb);
            (fit, e)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored
        .into_iter()
        .map(|(fit, mut e)| {
            if let Some(obj) = e.as_object_mut() {
                obj.insert("recipe_fit_score".to_string(), serde_json::json!(fit));
            }
            e
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_embedded_recipes() {
        assert_eq!(registry().len(), RECIPE_BLOBS.len());
    }

    #[test]
    fn filter_by_category() {
        let rag = filter_recipes(Some("rag"), None, None, "en");
        assert!(rag.len() >= 3);
        assert!(rag
            .iter()
            .all(|v| v.get("category").and_then(|c| c.as_str()) == Some("rag")));
    }

    #[test]
    fn get_known_recipe() {
        let r = get_recipe("compare-blind-test").expect("recipe");
        assert_eq!(r.category, "evaluation");
        assert!(r.prompt_template.is_some());
    }
}
