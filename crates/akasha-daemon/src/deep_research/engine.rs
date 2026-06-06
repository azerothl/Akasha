//! IterResearch engine: plan → search → extract → synthesize → stop → final report.

use super::parse::{
    assess_report_quality, extract_og_image_from_html, is_low_quality_summary,
    is_unreadable_fetched_content, parse_category, parse_json_array, parse_json_object,
    parse_search_results, parse_stop_decision, strip_html, truncate_content,
    unique_domains_from_urls, word_count,
};
use super::queries::{booster_queries, specialized_site_queries};
use super::prompts::{
    category_prompt_override, current_date_context, CATEGORY_CLASSIFY_PROMPT, EXTRACTOR_PROMPT,
    FINAL_REPORT_PROMPT, FINAL_REPORT_RETRY_PROMPT, QUERY_GEN_PROMPT, REPORT_TITLE_PROMPT,
    RESEARCH_PLAN_PROMPT, STOP_PROMPT, SYNTHESIZE_PROMPT,
};
use super::store::ResearchStore;
use super::types::{
    DeepResearchRun, EngineConfig, ReportMeta, ResearchBranch, ResearchFinding, ResearchPhase,
    ResearchSource, ResearchSubAgent, SkippedSource, StepStatus,
};
use akasha_llm::provider::CompletionRequest;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone)]
pub struct ResearchContext {
    pub llm_router: Arc<akasha_llm::LLMRouter>,
    pub tools_executor: Arc<tokio::sync::RwLock<Arc<akasha_tools::ToolExecutor>>>,
    pub data_dir: std::path::PathBuf,
}

impl ResearchContext {
    pub async fn executor(&self) -> Arc<akasha_tools::ToolExecutor> {
        self.tools_executor.read().await.clone()
    }

    pub fn user_context(&self) -> String {
        let profile = crate::user_profile::UserProfile::load(&self.data_dir);
        format!("{}{}", profile.format_for_prompt(), current_date_context())
    }

    pub async fn web_search_enabled(&self) -> bool {
        let ex = self.executor().await;
        akasha_tools::any_provider_available(&ex.policy)
    }
}

pub fn spawn_research_run(
    ctx: ResearchContext,
    topic: String,
    config: EngineConfig,
) -> String {
    let run_id = format!("dr_{}", Uuid::new_v4().simple());
    let cancel = Arc::new(AtomicBool::new(false));
    let store = ResearchStore::global();
    let started_at = chrono::Utc::now().to_rfc3339();

    let run = DeepResearchRun {
        id: run_id.clone(),
        topic: topic.clone(),
        phase: ResearchPhase::Planning,
        round: 0,
        max_rounds: config.max_rounds,
        branches: Vec::new(),
        evolving_report: String::new(),
        findings: Vec::new(),
        category: "general".to_string(),
        providers_used: Vec::new(),
        research_plan: String::new(),
        plan_sub_questions: Vec::new(),
        report_markdown: None,
        report_meta: None,
        error: None,
        search_degraded: false,
        started_at,
        finished_at: None,
    };

    let run_id_spawn = run_id.clone();
    let cancel_spawn = cancel.clone();
    tokio::spawn(async move {
        store.insert(run, cancel_spawn).await;
        if let Err(e) = run_engine(&ctx, &run_id_spawn, &topic, &config, &cancel).await {
            store
                .update(&run_id_spawn, |r| {
                    r.phase = ResearchPhase::Error;
                    r.error = Some(e);
                    r.finished_at = Some(chrono::Utc::now().to_rfc3339());
                })
                .await;
        }
    });

    run_id
}

async fn run_engine(
    ctx: &ResearchContext,
    run_id: &str,
    topic: &str,
    config: &EngineConfig,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let store = ResearchStore::global();
    let started = Instant::now();
    let user_ctx = ctx.user_context();
    let search_ok = ctx.web_search_enabled().await;

    store
        .update(run_id, |r| {
            r.search_degraded = !search_ok;
        })
        .await;

    // --- Planning ---
    set_phase(run_id, ResearchPhase::Planning).await;
    let branch_plan = add_branch(run_id, "Planning", "branch_plan").await;
    let sa_planner = add_sub_agent(run_id, &branch_plan, "planner", "Research plan", StepStatus::Running).await;

    let plan = create_plan(ctx, topic, &user_ctx).await;
    store
        .update(run_id, |r| {
            r.research_plan = plan.text.clone();
            r.plan_sub_questions = plan.sub_questions.clone();
        })
        .await;
    finish_sub_agent(run_id, &branch_plan, &sa_planner, StepStatus::Done).await;

    let sa_classifier = add_sub_agent(run_id, &branch_plan, "classifier", "Category", StepStatus::Running).await;
    let category = classify_category(ctx, topic, &user_ctx).await;
    store
        .update(run_id, |r| {
            r.category = category.clone();
        })
        .await;
    finish_sub_agent(run_id, &branch_plan, &sa_classifier, StepStatus::Done).await;
    finish_branch(run_id, &branch_plan, StepStatus::Done).await;

    let mut report = String::new();
    let mut findings: Vec<ResearchFinding> = Vec::new();
    let mut urls_fetched: HashSet<String> = HashSet::new();
    let mut queries_used: HashSet<String> = HashSet::new();
    let mut queries_log: Vec<String> = Vec::new();
    let mut skipped_sources: Vec<SkippedSource> = Vec::new();
    let mut search_hits_seen: u32 = 0;
    let mut providers_used: Vec<String> = Vec::new();
    let mut consecutive_empty = 0u32;

    for round_num in 1..=config.max_rounds {
        if cancel.load(Ordering::SeqCst) {
            store
                .update(run_id, |r| {
                    r.phase = ResearchPhase::Cancelled;
                    r.finished_at = Some(chrono::Utc::now().to_rfc3339());
                })
                .await;
            return Ok(());
        }
        if started.elapsed().as_secs() >= config.max_time_secs {
            break;
        }

        store
            .update(run_id, |r| {
                r.round = round_num;
            })
            .await;

        let branch_id = format!("round_{round_num}");
        let branch_label = format!("Round {round_num}");
        add_branch(run_id, &branch_label, &branch_id).await;

        // --- Query generation ---
        set_phase(run_id, ResearchPhase::Searching).await;
        let sa_query = add_sub_agent(
            run_id,
            &branch_id,
            "query_gen",
            "Generating queries",
            StepStatus::Running,
        )
        .await;

        let category = store
            .get(run_id)
            .await
            .map(|r| r.category.clone())
            .unwrap_or_else(|| "general".to_string());
        let pages_so_far = urls_fetched.len() as u32;
        let queries = generate_queries(
            ctx,
            topic,
            &plan.text,
            &report,
            round_num,
            &category,
            pages_so_far,
            &user_ctx,
            &mut queries_used,
        )
        .await;

        finish_sub_agent(run_id, &branch_id, &sa_query, StepStatus::Done).await;

        if queries.is_empty() {
            finish_branch(run_id, &branch_id, StepStatus::Error).await;
            break;
        }

        for q in &queries {
            queries_log.push(q.clone());
        }

        let mut round_findings: Vec<ResearchFinding> = Vec::new();

        if search_ok {
            set_phase(run_id, ResearchPhase::Searching).await;
            let round_url_start = urls_fetched.len();
            for query in &queries {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let sa_search = add_sub_agent(
                    run_id,
                    &branch_id,
                    "search",
                    &truncate_label(query, 48),
                    StepStatus::Running,
                )
                .await;

                let (hits, provider_name) = web_search(ctx, query, config).await;
                if let Some(provider) = provider_name {
                    if !providers_used.contains(&provider) {
                        providers_used.push(provider);
                        store
                            .update(run_id, |r| {
                                r.providers_used = providers_used.clone();
                            })
                            .await;
                    }
                } else if !hits.is_empty() && !providers_used.contains(&"web".to_string()) {
                    providers_used.push("web".to_string());
                    store
                        .update(run_id, |r| {
                            r.providers_used = providers_used.clone();
                        })
                        .await;
                }
                finish_sub_agent(run_id, &branch_id, &sa_search, StepStatus::Done).await;

                set_phase(run_id, ResearchPhase::Reading).await;
                let mut urls_this_query = 0u32;
                for (title, url, _desc) in hits {
                    search_hits_seen += 1;
                    if urls_fetched.contains(&url) {
                        push_skipped_source(
                            &mut skipped_sources,
                            &url,
                            &title,
                            "duplicate",
                        );
                        continue;
                    }
                    if urls_this_query >= config.max_urls_per_query {
                        push_skipped_source(
                            &mut skipped_sources,
                            &url,
                            &title,
                            "per_query_limit",
                        );
                        continue;
                    }
                    if urls_fetched.len().saturating_sub(round_url_start)
                        >= config.max_total_urls_per_round as usize
                    {
                        push_skipped_source(
                            &mut skipped_sources,
                            &url,
                            &title,
                            "round_limit",
                        );
                        continue;
                    }
                    urls_this_query += 1;
                    urls_fetched.insert(url.clone());

                    let sa_reader = add_sub_agent(
                        run_id,
                        &branch_id,
                        "reader",
                        &truncate_label(&url, 40),
                        StepStatus::Running,
                    )
                    .await;

                    match fetch_and_extract(ctx, &url, &title, topic, config).await {
                        Some(finding) => {
                            round_findings.push(finding.clone());
                            store
                                .update(run_id, |r| {
                                    r.findings.push(finding);
                                })
                                .await;
                        }
                        None => {
                            push_skipped_source(
                                &mut skipped_sources,
                                &url,
                                &title,
                                "fetch_or_low_quality",
                            );
                        }
                    }
                    finish_sub_agent(run_id, &branch_id, &sa_reader, StepStatus::Done).await;
                }
            }
        } else {
            // Degraded: LLM-only step per query
            for query in &queries {
                let sa_analyst = add_sub_agent(
                    run_id,
                    &branch_id,
                    "analyst",
                    &truncate_label(query, 48),
                    StepStatus::Running,
                )
                .await;
                let body = degraded_research_step(ctx, topic, query, &user_ctx).await;
                let finding = ResearchFinding {
                    url: String::new(),
                    title: query.clone(),
                    summary: body.clone(),
                    evidence: Some(body),
                    image_url: None,
                };
                round_findings.push(finding.clone());
                store
                    .update(run_id, |r| {
                        r.findings.push(finding);
                    })
                    .await;
                finish_sub_agent(run_id, &branch_id, &sa_analyst, StepStatus::Done).await;
            }
        }

        if round_findings.is_empty() {
            consecutive_empty += 1;
            finish_branch(run_id, &branch_id, StepStatus::Error).await;
            if consecutive_empty >= config.max_empty_rounds {
                break;
            }
            continue;
        }
        consecutive_empty = 0;

        // --- Synthesize ---
        set_phase(run_id, ResearchPhase::Analyzing).await;
        let sa_analyst = add_sub_agent(
            run_id,
            &branch_id,
            "analyst",
            "Synthesizing",
            StepStatus::Running,
        )
        .await;
        findings.extend(round_findings);
        report = synthesize_report(ctx, topic, &findings, &report, config, &user_ctx).await;
        store
            .update(run_id, |r| {
                r.evolving_report = report.clone();
            })
            .await;
        finish_sub_agent(run_id, &branch_id, &sa_analyst, StepStatus::Done).await;
        finish_branch(run_id, &branch_id, StepStatus::Done).await;

        // --- Stop decision ---
        if round_num >= config.min_rounds {
            let sa_critic = add_sub_agent(
                run_id,
                &branch_id,
                "critic",
                "Completeness check",
                StepStatus::Running,
            )
            .await;
            let source_count = urls_fetched.len() as u32;
            let unique_domains = unique_domains_from_urls(urls_fetched.iter());
            if should_stop(
                ctx,
                topic,
                &report,
                round_num,
                source_count,
                unique_domains,
                config,
                &user_ctx,
            )
            .await
            {
                finish_sub_agent(run_id, &branch_id, &sa_critic, StepStatus::Done).await;
                break;
            }
            finish_sub_agent(run_id, &branch_id, &sa_critic, StepStatus::Done).await;
        }
    }

    // --- Booster gathering if evidence is still thin (web available) ---
    if search_ok && urls_fetched.len() < config.min_sources_for_report as usize {
        let category = store
            .get(run_id)
            .await
            .map(|r| r.category.clone())
            .unwrap_or_else(|| "general".to_string());
        let extra = booster_queries(topic, &category, urls_fetched.len() as u32);
        if !extra.is_empty() && started.elapsed().as_secs() + 90 < config.max_time_secs {
            gather_urls_for_queries(
                ctx,
                run_id,
                topic,
                config,
                cancel,
                &extra,
                &mut urls_fetched,
                &mut queries_log,
                &mut queries_used,
                &mut skipped_sources,
                &mut search_hits_seen,
                &mut providers_used,
                &mut findings,
                &mut report,
                &user_ctx,
            )
            .await;
        }
    }

    // Sync findings from store (authoritative)
    if let Some(run) = store.get(run_id).await {
        findings = run.findings;
        if report.is_empty() && !run.evolving_report.is_empty() {
            report = run.evolving_report;
        }
    }

    // --- Final report ---
    set_phase(run_id, ResearchPhase::Writing).await;
    let branch_final = add_branch(run_id, "Final report", "branch_final").await;
    let sa_writer = add_sub_agent(
        run_id,
        &branch_final,
        "writer",
        "Writing report",
        StepStatus::Running,
    )
    .await;

    let category = store.get(run_id).await.map(|r| r.category).unwrap_or_else(|| "general".to_string());
    let search_degraded = store.get(run_id).await.map(|r| r.search_degraded).unwrap_or(false);
    let round_count = store.get(run_id).await.map(|r| r.round).unwrap_or(0);

    let final_report = if report.is_empty() && !findings.is_empty() {
        fallback_report(topic, &findings)
    } else if report.is_empty() {
        if search_degraded {
            "No information could be gathered. Enable web search (web_search_enabled: true in tools_policy.yaml; Brave key optional — SearXNG/DuckDuckGo work without keys).".to_string()
        } else {
            "No information could be gathered for this question.".to_string()
        }
    } else {
        let manifest = format_source_manifest(&findings);
        let mut md = final_report(
            ctx,
            topic,
            &report,
            &category,
            &manifest,
            config.min_final_report_words,
            &user_ctx,
        )
        .await;
        let pages = findings.iter().filter(|f| !f.url.is_empty()).count();
        let sources_ok = !search_degraded && pages >= config.min_sources_for_report as usize;
        let quality = assess_report_quality(
            &md,
            pages,
            sources_ok,
            search_degraded,
            config.min_final_report_words,
        );
        if quality.needs_retry {
            let issues = format!(
                "- Word count {} (target {})\n- Inline link citations {} (target at least {})\n- Add citations from the source manifest for major claims",
                quality.words,
                config.min_final_report_words,
                quality.citations,
                pages.min(14)
            );
            md = final_report_retry(ctx, topic, &md, &manifest, &issues, &user_ctx).await;
        }
        md
    };

    let sources: Vec<ResearchSource> = findings
        .iter()
        .filter(|f| !f.url.is_empty())
        .map(|f| ResearchSource {
            title: f.title.clone(),
            url: f.url.clone(),
            og_image: f.image_url.clone(),
        })
        .collect();

    let plan_sub_questions = store
        .get(run_id)
        .await
        .map(|r| r.plan_sub_questions.clone())
        .unwrap_or_default();
    let images_retrieved = findings
        .iter()
        .filter(|f| f.image_url.is_some())
        .count() as u32;
    let pages_fetched = urls_fetched.len() as u32;
    let report_title =
        generate_report_title(ctx, topic, &category, &final_report, &user_ctx).await;

    let duration_secs = started.elapsed().as_secs();
    let pages_for_meta = pages_fetched.max(sources.len() as u32);
    let unique_domains = unique_domains_from_urls(sources.iter().map(|s| s.url.as_str()));
    let sources_sufficient =
        search_degraded || pages_for_meta >= config.min_sources_for_report;
    let mut quality_warnings = assess_report_quality(
        &final_report,
        sources.len(),
        sources_sufficient,
        search_degraded,
        config.min_final_report_words,
    )
    .warnings;
    if queries_log.is_empty() && !search_degraded {
        quality_warnings.push(
            "No search queries were recorded — methodology trace may be incomplete.".to_string(),
        );
    }
    tracing::info!(
        run_id = run_id,
        query_count = queries_log.len(),
        pages_fetched = pages_for_meta,
        "deep research completed"
    );
    let meta = ReportMeta {
        report_title: report_title.clone(),
        category: category.clone(),
        word_count: word_count(&final_report),
        sources,
        providers_used: providers_used.clone(),
        rounds: round_count,
        duration_secs,
        search_degraded,
        user_question: topic.to_string(),
        search_queries: queries_log,
        sub_questions: plan_sub_questions,
        pages_fetched: pages_for_meta,
        search_hits_seen,
        images_retrieved,
        sources_skipped: skipped_sources,
        unique_domains: unique_domains as u32,
        sources_sufficient,
        quality_warnings,
    };

    finish_sub_agent(run_id, &branch_final, &sa_writer, StepStatus::Done).await;
    finish_branch(run_id, &branch_final, StepStatus::Done).await;

    store
        .update(run_id, |r| {
            r.report_markdown = Some(final_report);
            r.report_meta = Some(meta);
            r.phase = ResearchPhase::Done;
            r.finished_at = Some(chrono::Utc::now().to_rfc3339());
        })
        .await;

    Ok(())
}

struct PlanResult {
    text: String,
    sub_questions: Vec<String>,
}

fn push_skipped_source(out: &mut Vec<SkippedSource>, url: &str, title: &str, reason: &str) {
    if out.len() >= 100 || url.is_empty() {
        return;
    }
    out.push(SkippedSource {
        url: url.to_string(),
        title: if title.is_empty() {
            url.to_string()
        } else {
            title.to_string()
        },
        reason: reason.to_string(),
    });
}

async fn create_plan(ctx: &ResearchContext, topic: &str, user_ctx: &str) -> PlanResult {
    let prompt = format!(
        "{user_ctx}{}",
        RESEARCH_PLAN_PROMPT.replace("{question}", topic)
    );
    let text = llm_complete(
        ctx,
        prompt,
        Some("Output valid JSON only.".to_string()),
        1024,
        0.3,
        "research",
    )
    .await
    .unwrap_or_default();

    if let Some(parsed) = parse_json_object(&text) {
        let mut parts = Vec::new();
        if !parsed.sub_questions.is_empty() {
            parts.push(format!(
                "Sub-questions: {}",
                parsed.sub_questions.join("; ")
            ));
        }
        if !parsed.key_topics.is_empty() {
            parts.push(format!("Key topics: {}", parsed.key_topics.join(", ")));
        }
        if !parsed.source_types.is_empty() {
            parts.push(format!("Source types: {}", parsed.source_types.join(", ")));
        }
        if let Some(sc) = parsed.success_criteria {
            parts.push(format!("Success: {sc}"));
        }
        if !parts.is_empty() {
            return PlanResult {
                text: parts.join("\n"),
                sub_questions: parsed.sub_questions,
            };
        }
    }
    PlanResult {
        text,
        sub_questions: Vec::new(),
    }
}

async fn classify_category(ctx: &ResearchContext, topic: &str, user_ctx: &str) -> String {
    let prompt = format!(
        "{user_ctx}{}",
        CATEGORY_CLASSIFY_PROMPT.replace("{question}", topic)
    );
    let text = llm_complete(ctx, prompt, None, 32, 0.0, "utility").await;
    parse_category(&text.unwrap_or_default())
}

async fn generate_queries(
    ctx: &ResearchContext,
    topic: &str,
    plan: &str,
    report: &str,
    round_num: u32,
    category: &str,
    pages_gathered: u32,
    user_ctx: &str,
    queries_used: &mut HashSet<String>,
) -> Vec<String> {
    const MAX_QUERIES_PER_ROUND: usize = 12;
    let (num_queries, round_instruction) = if round_num == 1 {
        (
            8,
            "First round — cast a WIDE net: sub-questions, specialized sites (site:), data/benchmarks, expert and community angles. Do not rely on a single generic query.",
        )
    } else if pages_gathered < 10 {
        (
            6,
            "Follow-up round — evidence is still thin: prioritize NEW domains and missing source types from the plan; avoid repeating prior queries.",
        )
    } else {
        (
            5,
            "Follow-up round — target under-covered angles and missing source types from the plan; go deeper, not broader repetition.",
        )
    };

    let prompt = format!(
        "{user_ctx}{}",
        QUERY_GEN_PROMPT
            .replace("{question}", topic)
            .replace("{research_plan}", plan)
            .replace("{report}", if report.is_empty() { "(No findings yet.)" } else { report })
            .replace("{round_num}", &round_num.to_string())
            .replace("{num_queries}", &num_queries.to_string())
            .replace("{pages_gathered}", &pages_gathered.to_string())
            .replace("{round_instruction}", round_instruction)
    );

    let text = llm_complete(
        ctx,
        prompt,
        Some("Output valid JSON array only.".to_string()),
        512,
        0.5,
        "research",
    )
    .await
    .unwrap_or_default();

    let mut queries: Vec<String> = parse_json_array(&text)
        .into_iter()
        .filter(|q| !q.trim().is_empty() && queries_used.insert(q.trim().to_lowercase()))
        .take(num_queries as usize)
        .collect();

    for sq in specialized_site_queries(topic, category, round_num) {
        if queries.len() >= MAX_QUERIES_PER_ROUND {
            break;
        }
        let key = sq.trim().to_lowercase();
        if !key.is_empty() && queries_used.insert(key) {
            queries.push(sq);
        }
    }

    queries
}

async fn web_search(
    ctx: &ResearchContext,
    query: &str,
    config: &EngineConfig,
) -> (Vec<(String, String, String)>, Option<String>) {
    let executor = ctx.executor().await;
    match executor
        .web_search(query, config.max_search_results.min(10))
        .await
    {
        Ok((body, res)) if res.success => {
            let provider = res
                .detail
                .as_deref()
                .and_then(|d| d.lines().find(|l| l.starts_with("provider: ")))
                .map(|l| l.trim_start_matches("provider: ").to_string());
            (parse_search_results(&body), provider)
        }
        _ => (Vec::new(), None),
    }
}

async fn fetch_and_extract(
    ctx: &ResearchContext,
    url: &str,
    title: &str,
    goal: &str,
    config: &EngineConfig,
) -> Option<ResearchFinding> {
    let executor = ctx.executor().await;
    let (body, res) = executor.web_fetch(url).await.ok()?;
    if !res.success {
        return None;
    }
    if is_unreadable_fetched_content(&body, url) {
        return None;
    }
    let og_image = extract_og_image_from_html(&body);
    let plain = strip_html(&body);
    let content = truncate_content(&plain, config.max_content_chars);
    if content.len() < 50 {
        return None;
    }

    let prompt = EXTRACTOR_PROMPT
        .replace("{goal}", goal)
        .replace("{webpage_content}", &content);

    let response = llm_complete(ctx, prompt, None, 3072, 0.2, "utility").await?;

    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&extract_json_object(&response)) {
        let summary = parsed
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if is_low_quality_summary(&summary) {
            return None;
        }
        let evidence = parsed
            .get("evidence")
            .and_then(|v| v.as_str())
            .map(String::from);
        let image_url = parsed
            .get("image_urls")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .find(|s| s.starts_with("https://"))
                    .map(String::from)
            })
            .or(og_image);
        return Some(ResearchFinding {
            url: url.to_string(),
            title: title.to_string(),
            summary,
            evidence,
            image_url,
        });
    }

    Some(ResearchFinding {
        url: url.to_string(),
        title: title.to_string(),
        summary: response.chars().take(500).collect(),
        evidence: Some(response.chars().take(3000).collect()),
        image_url: og_image,
    })
}

fn extract_json_object(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}

async fn synthesize_report(
    ctx: &ResearchContext,
    topic: &str,
    findings: &[ResearchFinding],
    current_report: &str,
    config: &EngineConfig,
    user_ctx: &str,
) -> String {
    let window = findings.len().min(config.synthesis_window);
    let slice = &findings[findings.len().saturating_sub(window)..];
    let findings_text = format_findings(slice);

    let prompt = format!(
        "{user_ctx}{}",
        SYNTHESIZE_PROMPT
            .replace("{question}", topic)
            .replace(
                "{report}",
                if current_report.is_empty() {
                    "(First round — no report yet.)"
                } else {
                    current_report
                },
            )
            .replace("{new_findings}", &findings_text)
    );

    llm_complete(ctx, prompt, None, 12_000, 0.3, "research")
        .await
        .unwrap_or_else(|| current_report.to_string())
}

async fn should_stop(
    ctx: &ResearchContext,
    topic: &str,
    report: &str,
    round_num: u32,
    source_count: u32,
    unique_domains: usize,
    config: &EngineConfig,
    user_ctx: &str,
) -> bool {
    if source_count < config.min_sources_before_stop
        || unique_domains < config.min_unique_domains_before_stop as usize
    {
        return false;
    }
    let prompt = format!(
        "{user_ctx}{}",
        STOP_PROMPT
            .replace("{question}", topic)
            .replace("{report}", report)
            .replace("{round_num}", &round_num.to_string())
            .replace("{source_count}", &source_count.to_string())
            .replace("{unique_domains}", &unique_domains.to_string())
            .replace(
                "{min_sources}",
                &config.min_sources_before_stop.to_string(),
            )
            .replace(
                "{min_domains}",
                &config.min_unique_domains_before_stop.to_string(),
            )
    );
    let text = llm_complete(ctx, prompt, None, 128, 0.2, "utility")
        .await
        .unwrap_or_default();
    parse_stop_decision(&text)
}

async fn generate_report_title(
    ctx: &ResearchContext,
    topic: &str,
    category: &str,
    report_excerpt: &str,
    user_ctx: &str,
) -> String {
    let excerpt: String = report_excerpt.chars().take(800).collect();
    let prompt = format!(
        "{user_ctx}{}",
        REPORT_TITLE_PROMPT
            .replace("{question}", topic)
            .replace("{category}", category)
            .replace(
                "{excerpt}",
                if excerpt.is_empty() {
                    "(none yet)"
                } else {
                    &excerpt
                },
            )
    );
    let raw = llm_complete(ctx, prompt, None, 80, 0.3, "utility")
        .await
        .unwrap_or_default();
    sanitize_report_title(&raw, topic)
}

fn sanitize_report_title(raw: &str, fallback_topic: &str) -> String {
    let mut t = raw.trim().trim_matches('"').trim_matches('\'').to_string();
    if let Some(first) = t.lines().next() {
        t = first.trim().to_string();
    }
    if t.ends_with('?') {
        t.pop();
        t = t.trim().to_string();
    }
    if t.len() < 8 || t.to_lowercase() == fallback_topic.trim().to_lowercase() {
        return fallback_title_from_topic(fallback_topic);
    }
    if t.len() > 120 {
        t.truncate(120);
        t = t.trim().to_string();
    }
    t
}

fn fallback_title_from_topic(topic: &str) -> String {
    let line = topic.lines().next().unwrap_or(topic).trim();
    let words: Vec<&str> = line.split_whitespace().take(12).collect();
    if words.is_empty() {
        return "Research report".to_string();
    }
    let mut s = words.join(" ");
    if line.contains('?') || line.len() < 80 {
        if !s.to_lowercase().starts_with("analysis of") && !s.to_lowercase().starts_with("analyse") {
            s = format!("Analysis: {s}");
        }
    }
    if s.len() > 100 {
        s.truncate(100);
    }
    s
}

fn format_source_manifest(findings: &[ResearchFinding]) -> String {
    let mut lines: Vec<String> = findings
        .iter()
        .filter(|f| !f.url.is_empty())
        .map(|f| format!("- [{}]({}) — {}", f.title, f.url, f.summary.replace('\n', " ")))
        .collect();
    lines.sort();
    lines.dedup();
    if lines.is_empty() {
        "(No web pages were successfully read.)".to_string()
    } else {
        lines.join("\n")
    }
}

async fn final_report(
    ctx: &ResearchContext,
    topic: &str,
    report: &str,
    category: &str,
    source_manifest: &str,
    min_words: usize,
    user_ctx: &str,
) -> String {
    let override_text = category_prompt_override(category);
    let prompt = format!(
        "{user_ctx}{}",
        FINAL_REPORT_PROMPT
            .replace("{question}", topic)
            .replace("{report}", report)
            .replace("{source_manifest}", source_manifest)
            .replace("{min_words}", &min_words.to_string())
            .replace("{category_override}", override_text)
    );
    llm_complete(
        ctx,
        prompt,
        Some(
            "Evidence-first markdown: ## headings, inline [title](url) citations from the manifest, ## Limitations and confidence, neutral tone, no greeting.".to_string(),
        ),
        16_384,
        0.35,
        "research",
    )
    .await
    .unwrap_or_else(|| report.to_string())
}

async fn final_report_retry(
    ctx: &ResearchContext,
    topic: &str,
    report: &str,
    source_manifest: &str,
    quality_issues: &str,
    user_ctx: &str,
) -> String {
    let prompt = format!(
        "{user_ctx}{}",
        FINAL_REPORT_RETRY_PROMPT
            .replace("{question}", topic)
            .replace("{report}", report)
            .replace("{source_manifest}", source_manifest)
            .replace("{quality_issues}", quality_issues)
    );
    llm_complete(
        ctx,
        prompt,
        Some("Full revised markdown only.".to_string()),
        16_384,
        0.3,
        "research",
    )
    .await
    .unwrap_or_else(|| report.to_string())
}

/// Run search+fetch for a list of queries (booster round); updates shared state.
async fn gather_urls_for_queries(
    ctx: &ResearchContext,
    run_id: &str,
    topic: &str,
    config: &EngineConfig,
    cancel: &Arc<AtomicBool>,
    queries: &[String],
    urls_fetched: &mut HashSet<String>,
    queries_log: &mut Vec<String>,
    queries_used: &mut HashSet<String>,
    skipped_sources: &mut Vec<SkippedSource>,
    search_hits_seen: &mut u32,
    providers_used: &mut Vec<String>,
    findings: &mut Vec<ResearchFinding>,
    report: &mut String,
    user_ctx: &str,
) {
    let store = ResearchStore::global();
    let branch_id = "booster_gather";
    add_branch(run_id, "Additional sources", branch_id).await;
    let mut round_findings: Vec<ResearchFinding> = Vec::new();

    for query in queries {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let key = query.trim().to_lowercase();
        if key.is_empty() || !queries_used.insert(key) {
            continue;
        }
        queries_log.push(query.clone());

        let (hits, provider_name) = web_search(ctx, query, config).await;
        if let Some(provider) = provider_name {
            if !providers_used.contains(&provider) {
                providers_used.push(provider);
            }
        }
        let mut urls_this_query = 0u32;
        for (title, url, _desc) in hits {
            *search_hits_seen += 1;
            if urls_fetched.contains(&url) {
                push_skipped_source(skipped_sources, &url, &title, "duplicate");
                continue;
            }
            if urls_this_query >= config.max_urls_per_query {
                push_skipped_source(skipped_sources, &url, &title, "per_query_limit");
                continue;
            }
            if urls_fetched.len() >= config.max_total_urls_per_round as usize * 2 {
                push_skipped_source(skipped_sources, &url, &title, "round_limit");
                continue;
            }
            urls_this_query += 1;
            urls_fetched.insert(url.clone());
            if let Some(finding) = fetch_and_extract(ctx, &url, &title, topic, config).await {
                round_findings.push(finding.clone());
                store
                    .update(run_id, |r| r.findings.push(finding))
                    .await;
            } else {
                push_skipped_source(skipped_sources, &url, &title, "fetch_or_low_quality");
            }
        }
    }

    if !round_findings.is_empty() {
        findings.extend(round_findings.clone());
        *report = synthesize_report(ctx, topic, findings, report, config, user_ctx).await;
        store
            .update(run_id, |r| r.evolving_report = report.clone())
            .await;
    }
    finish_branch(run_id, branch_id, StepStatus::Done).await;
}

async fn degraded_research_step(
    ctx: &ResearchContext,
    topic: &str,
    query: &str,
    user_ctx: &str,
) -> String {
    let prompt = format!(
        "{user_ctx}Research topic: {topic}\n\nAnswer this sub-question with structured notes. \
         Cite sources as [Source: title — url] when inferring from general knowledge. \
         Note that live web search is unavailable.\n\n{query}"
    );
    llm_complete(ctx, prompt, None, 1200, 0.5, "research")
        .await
        .unwrap_or_else(|| "(step failed)".to_string())
}

fn format_findings(findings: &[ResearchFinding]) -> String {
    findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let url_part = if f.url.is_empty() {
                String::new()
            } else {
                format!(" ({})", f.url)
            };
            let img_part = f
                .image_url
                .as_ref()
                .map(|u| format!("\nImage: {u}"))
                .unwrap_or_default();
            format!(
                "### Finding {} — {}{}{}\n{}\n",
                i + 1,
                f.title,
                url_part,
                img_part,
                f.evidence.as_deref().unwrap_or(&f.summary)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fallback_report(topic: &str, findings: &[ResearchFinding]) -> String {
    format!(
        "# Research: {topic}\n\n## Gathered findings\n\n{}",
        format_findings(findings)
    )
}

async fn llm_complete(
    ctx: &ResearchContext,
    prompt: String,
    system_prompt: Option<String>,
    max_tokens: u32,
    temperature: f32,
    task_type: &str,
) -> Option<String> {
    let req = CompletionRequest {
        prompt,
        max_tokens: Some(max_tokens),
        temperature: Some(temperature),
        preferred_task_type: Some(task_type.to_string()),
        system_prompt,
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
    ctx.llm_router.complete(&req).await.ok().map(|r| r.text)
}

// --- Store helpers for branches / sub-agents ---

async fn set_phase(run_id: &str, phase: ResearchPhase) {
    ResearchStore::global()
        .update(run_id, |r| r.phase = phase)
        .await;
}

async fn add_branch(run_id: &str, label: &str, id: &str) -> String {
    let branch = ResearchBranch {
        id: id.to_string(),
        label: label.to_string(),
        status: StepStatus::Running,
        sub_agents: Vec::new(),
    };
    ResearchStore::global()
        .update(run_id, |r| r.branches.push(branch))
        .await;
    id.to_string()
}

async fn finish_branch(run_id: &str, branch_id: &str, status: StepStatus) {
    ResearchStore::global()
        .update(run_id, |r| {
            if let Some(b) = r.branches.iter_mut().find(|b| b.id == branch_id) {
                b.status = status;
            }
        })
        .await;
}

async fn add_sub_agent(
    run_id: &str,
    branch_id: &str,
    role: &str,
    label: &str,
    status: StepStatus,
) -> String {
    let id = format!("{branch_id}_{role}_{}", Uuid::new_v4().simple());
    let agent = ResearchSubAgent {
        id: id.clone(),
        parent_branch_id: branch_id.to_string(),
        role: role.to_string(),
        label: label.to_string(),
        status,
    };
    ResearchStore::global()
        .update(run_id, |r| {
            if let Some(b) = r.branches.iter_mut().find(|b| b.id == branch_id) {
                b.sub_agents.push(agent);
            }
        })
        .await;
    id
}

async fn finish_sub_agent(run_id: &str, branch_id: &str, agent_id: &str, status: StepStatus) {
    ResearchStore::global()
        .update(run_id, |r| {
            if let Some(b) = r.branches.iter_mut().find(|b| b.id == branch_id) {
                if let Some(a) = b.sub_agents.iter_mut().find(|a| a.id == agent_id) {
                    a.status = status;
                }
            }
        })
        .await;
}

fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
