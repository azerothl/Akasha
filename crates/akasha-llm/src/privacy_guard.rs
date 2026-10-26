use crate::{CompletionRequest, CompletionResponse, LLMProvider};
use crate::provider::ProviderError;
use std::{sync::Arc, time::Duration};

pub(crate) struct Guard {
    pub inner: Arc<dyn LLMProvider>,
}
impl Guard {
    fn endpoint(&self) -> Option<String> {
        self.inner.endpoint().map(str::to_owned)
    }
    fn local(&self) -> bool {
        if let Some(url) = self.endpoint() { return akasha_core::privacy::loopback_url(&url); }
        self.inner.is_local()
    }
    fn check(&self) -> Result<(), ProviderError> {
        if self.local() { return Ok(()); }
        let recipient = format!("model:{}:{}", self.inner.name(), self.endpoint().map(|url|akasha_core::privacy::endpoint_identity(&url)).unwrap_or_else(||"default".into()));
        // Conservatively cover all context assembled by the runtime. A narrow
        // grant is never silently expanded by history, retrieval or tool results.
        akasha_core::privacy::check(&recipient, &["message","history","memory","documents","tool_results"])
            .map_err(ProviderError::Api)
    }
}
#[async_trait::async_trait]
impl LLMProvider for Guard {
    fn endpoint(&self) -> Option<&str> {self.inner.endpoint()}
    fn name(&self) -> &str { self.inner.name() }
    fn is_local(&self) -> bool { self.local() }
    fn is_available(&self) -> bool { self.check().is_ok() && self.inner.is_available() }
    fn supports_streaming(&self) -> bool { self.inner.supports_streaming() }
    fn get_cost(&self, usage:&crate::provider::TokenUsage) -> f64 {self.inner.get_cost(usage)}
    async fn complete(&self, request:&CompletionRequest, timeout:Duration, model:Option<&str>) -> Result<CompletionResponse,ProviderError> {
        self.check()?;
        self.inner.complete(request,timeout,model).await
    }
    async fn complete_stream(&self, request:&CompletionRequest, timeout:Duration, model:Option<&str>, tx:std::sync::mpsc::Sender<String>) -> Result<CompletionResponse,ProviderError> {
        self.check()?;
        self.inner.complete_stream(request,timeout,model,tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct Recording {calls:Arc<AtomicUsize>}
    #[async_trait::async_trait]
    impl LLMProvider for Recording {
        fn name(&self)->&str {"remote-test"}
        fn endpoint(&self)->Option<&str>{Some("https://service.example/v1")}
        fn is_available(&self)->bool {true}
        async fn complete(&self,_:&CompletionRequest,_:Duration,_:Option<&str>)->Result<CompletionResponse,ProviderError>{
            self.calls.fetch_add(1,Ordering::SeqCst);
            Ok(serde_json::from_value(serde_json::json!({"text":"actual result","model_used":"test"})).unwrap())
        }
    }
    #[tokio::test] async fn transport_is_never_called_without_scoped_consent() {
        let dir=tempfile::tempdir().unwrap();let calls=Arc::new(AtomicUsize::new(0));
        let guard=Guard {inner:Arc::new(Recording{calls:calls.clone()})};
        let request=serde_json::from_value(serde_json::json!({"prompt":"private"})).unwrap();
        assert!(guard.complete(&request,Duration::from_secs(1),None).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),0);
        let scope=akasha_core::privacy::Scope {data_dir:dir.path().into(),id:"task:test".into()};
        akasha_core::privacy::update(dir.path(),|state|{state.grants.push(akasha_core::privacy::Grant {scope:scope.id.clone(),recipient:"model:remote-test:https://service.example/v1".into(),data_classes:vec!["message","history","memory","documents","tool_results"].into_iter().map(str::to_owned).collect(),created_at:chrono::Utc::now().to_rfc3339(),expires_at:(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),revoked:false});Ok(())}).unwrap();
        assert!(akasha_core::privacy::scoped(scope.clone(),guard.complete(&request,Duration::from_secs(1),None)).await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst),1);
        akasha_core::privacy::update(dir.path(),|state|{state.grants[0].revoked=true;Ok(())}).unwrap();
        assert!(akasha_core::privacy::scoped(scope,guard.complete(&request,Duration::from_secs(1),None)).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),1);
    }
}
