//! Spec Loader - Parse and validate YAML specifications from spec/

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecLoaderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Event model from spec/09_event_model.yaml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventModel {
    pub events: Vec<EventDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDefinition {
    pub id: String,
}

/// Data model from spec/10_data_model.yaml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataModel {
    pub entities: HashMap<String, HashMap<String, serde_yaml::Value>>,
}

/// State machine from spec/11_state_machine.yaml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMachine {
    pub states: Vec<String>,
    pub transitions: Vec<Transition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub from: String,
    pub to: String,
}

/// Loaded specifications
#[derive(Debug, Clone, Default)]
pub struct Specs {
    pub event_model: Option<EventModel>,
    pub data_model: Option<DataModel>,
    pub state_machine: Option<StateMachine>,
}

pub struct SpecLoader {
    spec_dir: std::path::PathBuf,
}

impl SpecLoader {
    /// Create a new SpecLoader with the given spec directory
    pub fn new<P: AsRef<Path>>(spec_dir: P) -> Self {
        Self {
            spec_dir: spec_dir.as_ref().to_path_buf(),
        }
    }

    /// Load event model from spec/09_event_model.yaml
    pub fn load_event_model(&self) -> Result<EventModel, SpecLoaderError> {
        let path = self.spec_dir.join("09_event_model.yaml");
        let content = std::fs::read_to_string(path)?;
        let model: EventModel = serde_yaml::from_str(&content)?;
        Ok(model)
    }

    /// Load data model from spec/10_data_model.yaml
    pub fn load_data_model(&self) -> Result<DataModel, SpecLoaderError> {
        let path = self.spec_dir.join("10_data_model.yaml");
        let content = std::fs::read_to_string(path)?;
        let data_model: DataModel = serde_yaml::from_str(&content)?;
        Ok(data_model)
    }

    /// Load state machine from spec/11_state_machine.yaml
    pub fn load_state_machine(&self) -> Result<StateMachine, SpecLoaderError> {
        let path = self.spec_dir.join("11_state_machine.yaml");
        let content = std::fs::read_to_string(path)?;
        let state_machine: StateMachine = serde_yaml::from_str(&content)?;
        Ok(state_machine)
    }

    /// Load all specifications
    pub fn load_all(&self) -> Result<Specs, SpecLoaderError> {
        let mut specs = Specs::default();

        if let Ok(em) = self.load_event_model() {
            specs.event_model = Some(em);
        }
        if let Ok(dm) = self.load_data_model() {
            specs.data_model = Some(dm);
        }
        if let Ok(sm) = self.load_state_machine() {
            specs.state_machine = Some(sm);
        }

        Ok(specs)
    }
}

/// Convenience function to load specs from the default spec directory
/// (relative to the project root or current working directory)
pub fn load_specs<P: AsRef<Path>>(spec_dir: P) -> Result<Specs, SpecLoaderError> {
    let loader = SpecLoader::new(spec_dir);
    loader.load_all()
}
