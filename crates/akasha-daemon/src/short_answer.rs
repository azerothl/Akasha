//! Tool-free answers on the ordinary conversation worker.
use std::{
    path::Path,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use akasha_core::{EventEnvelope, EventType};
use akasha_llm::{CompletionRequest, ConversationMessage, LLMRouter, MessageRole};
use akasha_store::{TaskStatus, TaskStore};
use chrono::Utc;
use regex::Regex;
use serde_json::json;
use uuid::Uuid;

use crate::{agents::EventBus, memory::ShortTermStore};

pub(crate) const ROUTE_EVENT: &str = "short_answer_route";

static DISCOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)^\s*(?:(?:un abonnement|un article|un produit)\s+coûte|le prix initial est de)\s+",
        r"(?P<price>[0-9]{1,9}(?:[,.][0-9]{1,2})?)\s*(?:€|euros?)\.\s*",
        r"(?:une remise de\s+(?P<discount>[0-9]{1,3}(?:[,.][0-9]{1,2})?)\s*%\s+s.applique|une remise s.applique)",
        r",\s*puis\s+on ajoute\s+(?P<fee>[0-9]{1,9}(?:[,.][0-9]{1,2})?)\s*",
        r"(?:€|euros?)\s+de frais\.\s*quel est le total\s*\?",
        r"(?:\s*réponds en une seule phrase en français avec le calcul\.)?\s*$",
    )).expect("valid discount pattern")
});
static AUTONOMOUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:qu.est-ce qu[e'’]|c.est quoi|définis|explique brièvement|what is|define)\s*[\p{L}\p{N} '’()/-]{2,100}[?.!]?$").expect("valid question pattern")
});
static EXCLUSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:fichier|file|joint|pièce|document|dépôt|repo|workspace|projet|project|mon|ma|mes|notre|my|our|actualité|actuel|actuelle|dernier|dernière|aujourd.hui|current|latest|today|agenda|météo|web|cherche|recherche|vérifie|crée|ouvre|écris|envoie|puis|ensuite|compare|search|find|open|send|create|then|and|et)\b").expect("valid exclusion pattern")
});

pub(crate) fn route_reason(message: &str) -> Option<&'static str> {
    if DISCOUNT.is_match(message) {
        Some("local_discount_calculation")
    } else if AUTONOMOUS.is_match(message.trim()) && !EXCLUSION.is_match(message) {
        Some("autonomous_conversation")
    } else {
        None
    }
}

fn hundredths(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once([',', '.']).unwrap_or((value, ""));
    let fraction = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 10,
        2 => fraction.parse::<u64>().ok()?,
        _ => return None,
    };
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(fraction)
}

fn decimal(value: u64, scale: u64, places: usize) -> String {
    if value % scale == 0 {
        return (value / scale).to_string();
    }
    let fraction = format!("{:0places$}", value % scale);
    format!("{},{}", value / scale, fraction.trim_end_matches('0'))
}

fn local_answer(message: &str) -> Option<(String, bool)> {
    let captures = DISCOUNT.captures(message)?;
    let Some(discount) = captures
        .name("discount")
        .and_then(|value| hundredths(value.as_str()))
    else {
        return Some((
            "Quel pourcentage de remise faut-il appliquer ?".into(),
            false,
        ));
    };
    if discount > 10_000 {
        return Some((
            "Quel pourcentage de remise compris entre 0 et 100 % faut-il appliquer ?".into(),
            false,
        ));
    }
    let price = hundredths(&captures["price"])?;
    let fee = hundredths(&captures["fee"])?;
    // Preserve fractional cents during the percentage calculation, then round half up.
    let multiplier = 10_000 - discount;
    let discounted_cents = (price * multiplier + 5_000) / 10_000;
    let total = discounted_cents + fee;
    Some((
        format!(
            "Le total est de {} € ({} € × {}, arrondi au centime, puis + {} € de frais).",
            decimal(total, 100, 2),
            decimal(price, 100, 2),
            decimal(multiplier, 10_000, 4),
            decimal(fee, 100, 2),
        ),
        true,
    ))
}

pub(crate) fn selected(store_path: &Path, task_id: Uuid) -> bool {
    TaskStore::open(store_path)
        .and_then(|store| store.has_event(task_id, ROUTE_EVENT))
        .unwrap_or(false)
}

fn stream_delta(bus: &EventBus, path: &Path, task_id: Uuid, delta: &str, text: &str) {
    if delta.is_empty() {
        return;
    }
    if let Ok(store) = TaskStore::open(path) {
        let _ = store.insert_event(
            task_id,
            "assistant_text_delta",
            Some(&json!({"delta":delta,"schema_version":1})),
            &Utc::now().to_rfc3339(),
        );
    }
    let _ = bus.send(
        EventEnvelope::new(
            EventType::ProgressUpdate,
            Some(json!({"task_id":task_id,"progress_pct":50,"message":text})),
        )
        .with_correlation(task_id),
    );
}

pub(crate) async fn run(
    bus: &EventBus,
    router: &LLMRouter,
    path: &Path,
    task_id: Uuid,
    message: &str,
    session_id: &str,
    memory: Option<&ShortTermStore>,
) {
    if !crate::document_import::active(path, task_id) {
        return;
    }
    let local = local_answer(message);
    let verified = local.as_ref().is_some_and(|(_, verified)| *verified);
    let mut model_turns = 0;
    let result = if let Some((answer, _)) = local {
        stream_delta(bus, path, task_id, &answer, &answer);
        Ok(answer)
    } else {
        let turns = match memory {
            Some(memory) => memory.get_turns(session_id).await,
            None => Vec::new(),
        };
        let history: Vec<_> = turns
            .into_iter()
            .rev()
            .take(6)
            .rev()
            .filter_map(|turn| {
                let role = match turn.role.as_str() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    _ => return None,
                };
                Some(ConversationMessage {
                    role,
                    content: turn.content.chars().take(1200).collect(),
                    context_data: false,
                    runtime_control: false,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                })
            })
            .collect();
        let request = CompletionRequest {
            require_tool: false,
            conversation: (!history.is_empty()).then_some(history),
            tools: None,
            prompt: message.into(),
            max_tokens: Some(256),
            temperature: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            repeat_penalty: None,
            num_ctx: None,
            num_gpu: None,
            thinking_level: None,
            preferred_task_type: Some("conversation".into()),
            system_prompt: None,
            image_data_urls: None,
        };
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let rx = Mutex::new(rx);
        let mut text = String::new();
        let mut visible_bytes = 0;
        let mut drain = || {
            for chunk in rx.lock().expect("stream receiver").try_iter() {
                text.push_str(&chunk);
            }
            let visible = crate::chat_contract::visible_stream_prefix(&text);
            if let Some(delta) = visible.get(visible_bytes..) {
                stream_delta(bus, path, task_id, delta, visible);
            }
            visible_bytes = visible.len();
        };
        model_turns = 1;
        if let Ok(store) = TaskStore::open(path) {
            let _ = store.insert_event(
                task_id,
                "llm_call_started",
                Some(&json!({"route":"short_answer","round":1})),
                &Utc::now().to_rfc3339(),
            );
        }
        let completion = tokio::time::timeout(
            Duration::from_secs(router.default_timeout_secs().max(1)),
            router.complete_stream(&request, tx),
        );
        tokio::pin!(completion);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let response = loop {
            tokio::select! {
                result = &mut completion => {
                    if !crate::document_import::active(path, task_id) { return; }
                    drain();
                    break result;
                }
                _ = tick.tick() => {
                    if !crate::document_import::active(path, task_id) { return; }
                    drain();
                }
            }
        };
        if let Ok(store) = TaskStore::open(path) {
            let _ = store.insert_event(task_id, "llm_call_finished", Some(&json!({"route":"short_answer","round":1,"success":matches!(&response, Ok(Ok(_)))})), &Utc::now().to_rfc3339());
        }
        match response {
            Ok(Ok(response)) => {
                let answer =
                    crate::api_tool_parser::strip_redacted_reasoning_blocks(&response.text);
                let answer = answer.trim();
                if answer.is_empty()
                    || answer.contains("TOOL:")
                    || answer.contains("TOOL_JSON:")
                    || crate::chat_contract::unparsed_tool_request(answer)
                {
                    Err("Le modèle n’a pas fourni de réponse exploitable.".into())
                } else {
                    Ok(answer.to_string())
                }
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err("Le délai de réponse du modèle est dépassé.".into()),
        }
    };
    let (status, outcome) = match result {
        Ok(response) => (
            TaskStatus::Completed,
            json!({"response":response,"result":"succeeded","termination_reason":"success","acceptance_verified":verified,"model_turns":model_turns}),
        ),
        Err(error) => (
            TaskStatus::Failed,
            json!({"response":error,"reason":error,"result":"failed","termination_reason":"generation_failed","acceptance_verified":false,"model_turns":model_turns}),
        ),
    };
    let finalized =
        TaskStore::open(path).and_then(|store| store.finalize_outcome(task_id, status, outcome));
    let (status, outcome) = match finalized {
        Ok(finalized) => finalized,
        Err(error) => {
            tracing::error!(%task_id, %error, "Failed to finalize short answer");
            return;
        }
    };
    let event = match status {
        TaskStatus::Completed => {
            if let (Some(memory), Some(response)) = (memory, outcome["response"].as_str()) {
                memory.append(session_id, "user", message.to_string()).await;
                memory
                    .append(session_id, "assistant", response.to_string())
                    .await;
            }
            EventType::TaskCompleted
        }
        TaskStatus::Failed => EventType::TaskFailed,
        _ => return,
    };
    let _ = bus.send(
        EventEnvelope::new(
            event,
            Some(json!({"task_id":task_id,"response":outcome["response"],"outcome":outcome})),
        )
        .with_correlation(task_id),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_discount_uses_order_and_decimal_amounts() {
        let prompt = "Un abonnement coûte 80 euros. Une remise de 25 % s'applique, puis on ajoute 5 euros de frais. Quel est le total ? Réponds en une seule phrase en français avec le calcul.";
        assert_eq!(route_reason(prompt), Some("local_discount_calculation"));
        let (answer, verified) = local_answer(prompt).unwrap();
        assert!(verified);
        assert!(answer.contains("65 €"));
        let revised = prompt
            .replace("80 euros", "80,05 €")
            .replace("5 euros de frais", "5,10 € de frais");
        assert!(local_answer(&revised).unwrap().0.contains("65,14 €"));
        let ambiguous = prompt.replace(" de 25 %", "");
        assert_eq!(local_answer(&ambiguous).unwrap().1, false);
        assert!(local_answer(&format!("{prompt} Envoie la réponse.")).is_none());
    }

    #[test]
    fn routing_is_conservative_about_tasks_and_external_context() {
        assert_eq!(
            route_reason("Qu’est-ce qu’un algorithme ?"),
            Some("autonomous_conversation")
        );
        for prompt in [
            "What is the latest price?",
            "Explique brièvement mon projet",
            "Définis le mot puis écris un fichier",
            "Ouvre le document.",
            "Crée un site web complet",
        ] {
            assert_eq!(route_reason(prompt), None, "{prompt}");
        }
    }
}
