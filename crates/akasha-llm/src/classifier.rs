//! Task type classifier — 6 types (spec 32, 13).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CodeGeneration,
    CreativeWriting,
    ScientificAnalysis,
    DataAnalysis,
    Conversation,
    SystemDiagnostic,
}

impl TaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CodeGeneration => "code_generation",
            Self::CreativeWriting => "creative_writing",
            Self::ScientificAnalysis => "scientific_analysis",
            Self::DataAnalysis => "data_analysis",
            Self::Conversation => "conversation",
            Self::SystemDiagnostic => "system_diagnostic",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "code_generation" => Some(Self::CodeGeneration),
            "creative_writing" => Some(Self::CreativeWriting),
            "scientific_analysis" => Some(Self::ScientificAnalysis),
            "data_analysis" => Some(Self::DataAnalysis),
            "conversation" => Some(Self::Conversation),
            "system_diagnostic" => Some(Self::SystemDiagnostic),
            _ => None,
        }
    }
}

/// Simple keyword-based classifier. Returns task_type and confidence 0.0..=1.0.
pub fn classify_task_type(prompt: &str) -> (TaskType, f32) {
    let lower = prompt.to_lowercase();
    // Order matters: more specific / context cues first (e.g. "diagnostic" before "code" so "documentation" doesn’t match code_generation)
    let keywords: &[(&str, TaskType)] = &[
        ("diagnostic", TaskType::SystemDiagnostic),
        ("system health", TaskType::SystemDiagnostic),
        ("health state", TaskType::SystemDiagnostic),
        ("runbook", TaskType::SystemDiagnostic),
        ("health", TaskType::SystemDiagnostic),
        ("status", TaskType::SystemDiagnostic),
        ("write code", TaskType::CodeGeneration),
        ("generate code", TaskType::CodeGeneration),
        ("code generation", TaskType::CodeGeneration),
        ("function", TaskType::CodeGeneration),
        ("debug", TaskType::CodeGeneration),
        ("implement", TaskType::CodeGeneration),
        ("script", TaskType::CodeGeneration),
        ("write a story", TaskType::CreativeWriting),
        ("poem", TaskType::CreativeWriting),
        ("article", TaskType::CreativeWriting),
        ("email", TaskType::CreativeWriting),
        ("paper", TaskType::ScientificAnalysis),
        ("research", TaskType::ScientificAnalysis),
        ("analyze data", TaskType::DataAnalysis),
        ("statistics", TaskType::DataAnalysis),
        ("chart", TaskType::DataAnalysis),
    ];
    for (kw, tt) in keywords {
        if lower.contains(kw) {
            return (*tt, 0.85);
        }
    }
    (TaskType::Conversation, 0.7)
}
