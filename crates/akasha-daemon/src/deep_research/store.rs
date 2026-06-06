//! In-memory store for active and recent Deep Research runs.

use super::types::DeepResearchRun;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

const MAX_RUNS: usize = 50;

pub struct ResearchStore {
    runs: RwLock<HashMap<String, DeepResearchRun>>,
    cancel_flags: RwLock<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
}

impl ResearchStore {
    pub fn global() -> Arc<Self> {
        static STORE: OnceLock<Arc<ResearchStore>> = OnceLock::new();
        STORE
            .get_or_init(|| {
                Arc::new(ResearchStore {
                    runs: RwLock::new(HashMap::new()),
                    cancel_flags: RwLock::new(HashMap::new()),
                })
            })
            .clone()
    }

    pub async fn insert(&self, run: DeepResearchRun, cancel: Arc<std::sync::atomic::AtomicBool>) {
        let id = run.id.clone();
        {
            let mut runs = self.runs.write().await;
            if runs.len() >= MAX_RUNS {
                let oldest = runs
                    .iter()
                    .filter(|(_, r)| r.finished_at.is_some())
                    .min_by_key(|(_, r)| r.started_at.clone())
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    runs.remove(&k);
                }
            }
            runs.insert(id.clone(), run);
        }
        self.cancel_flags.write().await.insert(id, cancel);
    }

    pub async fn get(&self, id: &str) -> Option<DeepResearchRun> {
        self.runs.read().await.get(id).cloned()
    }

    pub async fn update<F>(&self, id: &str, f: F)
    where
        F: FnOnce(&mut DeepResearchRun),
    {
        if let Some(run) = self.runs.write().await.get_mut(id) {
            f(run);
        }
    }

    pub async fn cancel_flag(&self, id: &str) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        self.cancel_flags.read().await.get(id).cloned()
    }

    pub async fn request_cancel(&self, id: &str) -> bool {
        if let Some(flag) = self.cancel_flag(id).await {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            self.update(id, |run| {
                if run.phase != super::types::ResearchPhase::Done
                    && run.phase != super::types::ResearchPhase::Error
                {
                    run.phase = super::types::ResearchPhase::Cancelled;
                }
            })
            .await;
            true
        } else {
            false
        }
    }
}
