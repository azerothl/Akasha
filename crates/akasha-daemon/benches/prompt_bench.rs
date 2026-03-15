//! Benchmarks for prompt construction and routing heuristics (no LLM calls).

use akasha_daemon::agents::orchestrator::build_decomposer_prompt;
use akasha_daemon::api::{agent_role_system_prompt, message_suggests_tool_only_action};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

const DECOMPOSER_MESSAGE: &str = "Prends une photo et enregistre-la";

const AGENT_TYPES: &[&str] = &[
    "code",
    "search",
    "financial",
    "documentalist",
    "project_manager",
    "technical_writer",
    "research",
    "security_audit",
    "creative",
];

const HEURISTIC_MESSAGES: &[&str] = &[
    "Prends une photo avec la caméra",
    "Quelle est la météo à Paris ?",
    "Sauvegarde ce code dans /tmp/foo.py",
    "Génère une image d'un lapin",
    "Écris un script Python qui lit un fichier",
    "écris un script qui prend une photo",
];

fn bench_build_decomposer_prompt(c: &mut Criterion) {
    c.bench_function("build_decomposer_prompt", |b| {
        b.iter(|| {
            let s = build_decomposer_prompt(black_box(DECOMPOSER_MESSAGE));
            black_box(s)
        })
    });
}

fn bench_agent_role_system_prompt(c: &mut Criterion) {
    c.bench_function("agent_role_system_prompt_all_types", |b| {
        b.iter(|| {
            for t in AGENT_TYPES {
                let s = agent_role_system_prompt(black_box(t));
                black_box(s);
            }
        })
    });
}

fn bench_message_suggests_tool_only_action(c: &mut Criterion) {
    c.bench_function("message_suggests_tool_only_action_batch", |b| {
        b.iter(|| {
            for msg in HEURISTIC_MESSAGES {
                let r = message_suggests_tool_only_action(black_box(msg));
                black_box(r);
            }
        })
    });
}

criterion_group!(
    benches,
    bench_build_decomposer_prompt,
    bench_agent_role_system_prompt,
    bench_message_suggests_tool_only_action
);
criterion_main!(benches);
