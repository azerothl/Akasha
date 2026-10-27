//! Full workflow evidence is independent of model prose and preliminary probes.
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::path::Path;
pub fn validate(reports: &[Value], current: &Value) -> Result<Value> {
    ensure!(
        reports.len() == 3,
        "Trois passages complets et consécutifs sont nécessaires"
    );
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../spec/dev/quality/consumer_beta_corpus.json"
    ))?;
    use sha2::{Digest, Sha256};
    let mut ids = std::collections::HashSet::new();
    let mut eligible = std::collections::BTreeSet::from([
        "conversation".to_string(),
        "documents".to_string(),
        "actions".to_string(),
    ]);
    let mut samples = Vec::new();
    let first_sequence=reports[0]["sequence_number"].as_u64().filter(|value|*value>0)
        .ok_or_else(||anyhow::anyhow!("Numéro de passage invalide"))?;
    for (index, report) in reports.iter().enumerate() {
        ensure!(report["schema_version"] == 1 && report["corpus_version"] == corpus["version"],
            "Le contrat du rapport est incomplet ou périmé");
        ensure!(
            report["series_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
                && report["series_id"] == reports[0]["series_id"]
                && report["sequence_number"].as_u64() == first_sequence.checked_add(index as u64)
                && (index == 0 || report["previous_run_id"] == reports[index - 1]["run_id"]),
            "Les trois passages doivent se suivre dans la même série"
        );
        ensure!(
            report["corpus_file_sha256"]
                == format!(
                    "{:x}",
                    Sha256::digest(include_bytes!(
                        "../../../spec/dev/quality/consumer_beta_corpus.json"
                    ))
                ),
            "Le corpus a changé : relancez la qualification"
        );
        ensure!(report["mode"]=="live"&&report["identity"]==*current,"Le binaire, moteur, modèle, matériel ou sa configuration ont changé ; relancez les trois passages réels");
        let id = report["run_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Passage sans identifiant"))?;
        ensure!(ids.insert(id), "Un passage a été fourni plusieurs fois");
        ensure!(
            report["complete"] == true && report["critical_failure"] == false,
            "Qualification incomplète ou défaut de confidentialité/intégrité"
        );
        let rows = report["cases"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Preuves de scénarios absentes"))?;
        ensure!(rows.len() == 50, "Le corpus complet de 50 cas est exigé");
        let mut total = 0;
        let mut successes = 0;
        let mut scores = std::collections::BTreeMap::<String, (usize, usize)>::new();
        for case in corpus["cases"].as_array().unwrap() {
            let row = rows
                .iter()
                .find(|row| row["id"] == case["id"])
                .ok_or_else(|| anyhow::anyhow!("Scénario absent"))?;
            let checks = &row["evidence"]["checks"];
            ensure!(matches!(row["evidence"]["observed"].as_str(),Some("result"|"refusal"|"failure"))
                && case["checks"].as_array().unwrap().iter().all(|name|checks[name.as_str().unwrap()].is_boolean())
                && row["evidence"]["receipt"].as_object().is_some_and(|receipt|!receipt.is_empty()),
                "Scénario partiellement observé : résultat, contrôles et reçu sont exigés");
            ensure!(
                checks["no_false_success"] == true,
                "Faux succès ou absence de contrôle indépendant"
            );
            for critical in ["privacy", "integrity"] {
                if case["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|check| check == critical)
                {
                    ensure!(
                        checks[critical] == true,
                        "Défaut de confidentialité ou d’intégrité"
                    );
                }
            }
            let passed = case["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|name| checks[name.as_str().unwrap()] == true)
                && row["evidence"]["observed"] == case["expect"];
            if case["category"] == "protection" {
                ensure!(case["checks"].as_array().unwrap().iter().all(|name|checks[name.as_str().unwrap()]==true),
                    "Échec d’un contrôle de protection : qualification refusée");
            }
            if row["evidence"]["model_executed"] == true {
                let elapsed = row["evidence"]["duration_ms"]
                    .as_u64()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| anyhow::anyhow!("Latence réelle absente"))?;
                samples.push(elapsed);
            }
            total += 1;
            successes += usize::from(passed);
            let usage = match case["category"].as_str().unwrap() {
                "conversation" => Some("conversation"),
                "documents" => Some("documents"),
                "organization" => Some("actions"),
                _ => None,
            };
            if let Some(usage) = usage {
                let entry = scores.entry(usage.into()).or_default();
                entry.0 += usize::from(passed);
                entry.1 += 1;
            }
        }
        ensure!(
            successes as f64 / total as f64 >= 0.95,
            "Moins de 95 % de réussite sur un passage"
        );
        eligible.retain(|usage| {
            scores
                .get(usage)
                .is_some_and(|score| score.0 as f64 / score.1 as f64 >= 0.95)
        });
    }
    // Each usage must meet its own threshold, not hide behind another usage.
    let usages: Vec<_> = eligible.into_iter().collect();
    ensure!(!usages.is_empty(), "Aucun usage qualifié");
    ensure!(!samples.is_empty(), "Latences des modèles absentes");
    samples.sort();
    let median = samples[samples.len() / 2];
    let fingerprints: Vec<_> = reports
        .iter()
        .map(|report| format!("{:x}", Sha256::digest(serde_json::to_vec(report).unwrap())))
        .collect();
    Ok(
        json!({"schema_version":1,"identity":current,"usages":usages,"report_sha256":fingerprints,"run_ids":ids,"qualified":true,"recommended":false,"median_latency_ms":median}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn reports() -> (Value, Vec<Value>) {
        let identity = json!({"model":"synthetic","model_digest":"abc"});
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../spec/dev/quality/consumer_beta_corpus.json"
        ))
        .unwrap();
        let rows:Vec<_>=corpus["cases"].as_array().unwrap().iter().map(|case|{let checks:serde_json::Map<String,Value>=case["checks"].as_array().unwrap().iter().map(|name|(name.as_str().unwrap().into(),true.into())).collect();json!({"id":case["id"],"evidence":{"observed":case["expect"],"checks":checks,"receipt":{"kind":"synthetic-unit-test"},"model_executed":true,"duration_ms":100}})}).collect();
        use sha2::{Digest, Sha256};
        let reports=(0..3).map(|index|json!({"schema_version":1,"corpus_version":corpus["version"],"mode":"live","identity":identity,"run_id":index.to_string(),"series_id":"synthetic-series","sequence_number":index+1,"previous_run_id":if index>0{Some((index-1).to_string())}else{None},"complete":true,"critical_failure":false,"corpus_file_sha256":format!("{:x}",Sha256::digest(include_bytes!("../../../spec/dev/quality/consumer_beta_corpus.json"))),"cases":rows})).collect();
        (identity, reports)
    }
    #[test]
    fn complete_proof_rejects_drift_duplicate_runs_and_hidden_privacy_failure() {
        let (current, mut reports) = reports();
        assert!(validate(&reports, &current).is_ok());
        reports[1]["run_id"] = reports[0]["run_id"].clone();
        assert!(validate(&reports, &current).is_err());
        reports[1]["run_id"] = "1".into();
        reports[0]["cases"][0]["evidence"]["checks"]["no_false_success"] = false.into();
        assert!(validate(&reports, &current).is_err());
        assert!(validate(&reports, &json!({"model":"different"})).is_err());
    }
    #[test]
    fn one_weak_usage_cannot_hide_behind_other_usages() {
        let (current, mut reports) = reports();
        reports[0]["cases"][10]["evidence"]["checks"]["grounding"] = false.into();
        let proof = validate(&reports, &current).unwrap();
        assert!(!proof["usages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|usage| usage == "documents"));
        assert!(proof["usages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|usage| usage == "conversation"));
    }
    #[test]
    fn cached_complete_flag_cannot_hide_partial_observations() {
        let (current,mut partial)=reports();
        partial[0]["cases"][0]["evidence"]=json!({"checks":{"no_false_success":true}});
        assert!(validate(&partial,&current).is_err());
        let (_,mut missing_receipt)=reports();
        missing_receipt[0]["cases"][0]["evidence"].as_object_mut().unwrap().remove("receipt");
        assert!(validate(&missing_receipt,&current).is_err());
        let (_,mut stale)=reports();stale[0]["corpus_version"]="obsolete".into();
        assert!(validate(&stale,&current).is_err());
    }
    #[test]
    fn every_protection_check_blocks_even_with_a_forged_noncritical_summary() {
        let corpus:Value=serde_json::from_str(include_str!("../../../spec/dev/quality/consumer_beta_corpus.json")).unwrap();
        for case in corpus["cases"].as_array().unwrap().iter().filter(|case|case["category"]=="protection") {
            for check in case["checks"].as_array().unwrap() {
                let (current,mut values)=reports();
                let row=values[0]["cases"].as_array_mut().unwrap().iter_mut().find(|row|row["id"]==case["id"]).unwrap();
                row["evidence"]["checks"][check.as_str().unwrap()]=false.into();
                assert_eq!(values[0]["critical_failure"],false);
                assert!(validate(&values,&current).is_err(),"{} {}",case["id"],check);
            }
        }
    }
    #[test]
    fn consecutive_window_may_start_after_an_earlier_failed_run() {
        let (current,mut values)=reports();
        for (index,row) in values.iter_mut().enumerate() {row["sequence_number"]=(index+2).into();}
        assert!(validate(&values,&current).is_ok());
        for start in [json!(0),json!(-1),json!(true),json!("2"),json!(2.0),json!(u64::MAX)] {
            let mut broken=values.clone();broken[0]["sequence_number"]=start;
            assert!(validate(&broken,&current).is_err());
        }
        values[1]["previous_run_id"]="unrelated".into();assert!(validate(&values,&current).is_err());
    }
}
pub async fn capabilities(data: &Path, router: &akasha_llm::LLMRouter) -> Value {
    let mut base = crate::model_qualification::capabilities(data, router);
    let mut qualified = Vec::new();
    let mut observations = Vec::new();
    let mut identities = std::collections::HashMap::new();
    if let Some(entries) = base["evidence"].as_array_mut() {
        for proof in entries {
            let identity = &proof["identity"];
            let provider = identity["provider"].as_str().unwrap_or("");
            let model = identity["model"].as_str().unwrap_or("");
            let key = format!("{provider}:{model}");
            if !identities.contains_key(&key) {
                identities.insert(
                    key.clone(),
                    crate::model_qualification::current_identity(router, data, provider, model)
                        .await
                        .ok(),
                );
            }
            let stale = identities[&key].as_ref() != Some(identity);
            proof["stale"] = stale.into();
            if stale {
                proof["qualification_status"] = "stale".into();
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(data.join("qualification/full")) {
        for entry in entries.flatten().take(50) {
            if entry.path().extension().is_none_or(|value| value != "json")
                || entry
                    .metadata()
                    .map_or(true, |meta| meta.len() > 4 * 1024 * 1024)
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(reports) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
                continue;
            };
            let Some(identity) = reports.first().map(|value| &value["identity"]) else {
                continue;
            };
            let provider = identity["provider"].as_str().unwrap_or("");
            let model = identity["model"].as_str().unwrap_or("");
            let key = format!("{provider}:{model}");
            if !identities.contains_key(&key) {
                identities.insert(
                    key.clone(),
                    crate::model_qualification::current_identity(router, data, provider, model)
                        .await
                        .ok(),
                );
            }
            let verified = identities[&key]
                .as_ref()
                .and_then(|current| validate(&reports, current).ok());
            let stale = verified.is_none();
            observations.push(json!({"identity":identity,"stale":stale}));
            if let Some(mut proof) = verified {
                if identity["hardware_profile"]["profile"]["ram_gb"]
                    .as_u64()
                    .is_some_and(|value| value > 0)
                    && identity["source_dirty"] == false
                {
                    proof["evidence_url"] = format!(
                        "/api/router/qualification-evidence/{}",
                        entry
                            .file_name()
                            .to_string_lossy()
                            .trim_end_matches(".json")
                    )
                    .into();
                    qualified.push(proof);
                }
            }
        }
    }
    qualified.sort_by(|a, b| {
        a["identity"]["model_bytes"]
            .as_u64()
            .unwrap_or(u64::MAX)
            .cmp(&b["identity"]["model_bytes"].as_u64().unwrap_or(u64::MAX))
            .then_with(|| {
                a["median_latency_ms"]
                    .as_u64()
                    .unwrap_or(u64::MAX)
                    .cmp(&b["median_latency_ms"].as_u64().unwrap_or(u64::MAX))
            })
    });
    let mut recommended = Vec::new();
    for usage in ["conversation", "documents", "actions"] {
        if let Some(proof) = qualified.iter().find(|proof| {
            proof["usages"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == usage))
        }) {
            recommended.push(json!({"usage":usage,"identity":proof["identity"],"evidence_url":proof["evidence_url"],"median_latency_ms":proof["median_latency_ms"],"report_sha256":proof["report_sha256"],"run_ids":proof["run_ids"]}));
        }
    }
    base["recommended_models"] = recommended.into();
    base["workflow_evidence"] = observations.into();
    base
}
pub async fn route(
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    data: &Path,
    router: &akasha_llm::LLMRouter,
) -> Option<String> {
    let prefix = "/api/router/qualification-evidence";
    if path != prefix && !path.starts_with(&format!("{prefix}/")) {
        return None;
    }
    let result = async {
        if method == "GET" {
            let id = uuid::Uuid::parse_str(path.trim_start_matches(&format!("{prefix}/")))?;
            let reports: Value = serde_json::from_slice(&std::fs::read(
                data.join("qualification/full").join(format!("{id}.json")),
            )?)?;
            return Ok::<Value, anyhow::Error>(reports);
        }
        ensure!(method == "POST", "Méthode invalide");
        let input: Value = serde_json::from_slice(body.unwrap_or_default())?;
        ensure!(
            input["confirmed"] == true,
            "Confirmez l’import des preuves synthétiques"
        );
        let reports = input["reports"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Rapports manquants"))?;
        let first = reports
            .first()
            .ok_or_else(|| anyhow::anyhow!("Trois rapports requis"))?;
        let current = crate::model_qualification::current_identity(
            router,
            data,
            first["identity"]["provider"].as_str().unwrap_or(""),
            first["identity"]["model"].as_str().unwrap_or(""),
        )
        .await?;
        let proof = validate(reports, &current)?;
        let dir = data.join("qualification/full");
        std::fs::create_dir_all(&dir)?;
        let id = uuid::Uuid::new_v4();
        let mut file = tempfile::NamedTempFile::new_in(&dir)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(reports)?)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(dir.join(format!("{id}.json")))?;
        Ok(json!({"proof":proof,"evidence_url":format!("{prefix}/{id}")}))
    }
    .await;
    Some(match result {
        Ok(value) => crate::api_http::json_response("200 OK", &value.to_string()),
        Err(error) => crate::api_http::json_response(
            "400 Bad Request",
            &json!({"error":error.to_string()}).to_string(),
        ),
    })
}
