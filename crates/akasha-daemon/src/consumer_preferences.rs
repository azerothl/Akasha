//! An explicit backup saves only these presentation choices, never UI secrets.
use anyhow::{ensure,Result};
use serde_json::{json,Value};
use std::path::Path;
pub fn filtered(input:&Value)->Value{
 let mut result=serde_json::Map::new();
 for key in ["akasha_locale","akasha_theme","akasha_ui_mode","akasha_ui_density","akasha_ui_chat_tips","akasha_ui_prompt_chips","akasha_ui_buddy","akasha_chat_dual_pane","akasha_companion_open","akasha_setup_wizard_done","akasha_onboarding_dismissed"] {if let Some(value)=input[key].as_str().filter(|value|value.len()<=80){result.insert(key.into(),value.into());}}
 Value::Object(result)
}
pub fn read(data:&Path)->Value{std::fs::read(data.join("consumer_settings.json")).ok().and_then(|bytes|serde_json::from_slice::<Value>(&bytes).ok()).map(|value|filtered(&value["preferences"])).unwrap_or(json!({}))}
pub fn route(method:&str,path:&str,body:Option<&[u8]>,data:&Path)->Option<String>{
 if path!="/api/consumer/preferences"||!matches!(method,"GET"|"POST"){return None;}
 let result=(||->Result<Value>{
  if method=="POST"{let value:Value=serde_json::from_slice(body.unwrap_or_default())?;ensure!(value["confirmed"]==true,"Confirmez la sauvegarde des préférences d’affichage");let value=filtered(&value["preferences"]);std::fs::create_dir_all(data)?;let mut file=tempfile::NamedTempFile::new_in(data)?;use std::io::Write;file.write_all(&serde_json::to_vec(&json!({"schema_version":1,"preferences":value}))?)?;file.as_file().sync_all()?;file.persist(data.join("consumer_settings.json"))?;}
  use sha2::{Digest,Sha256};let id=format!("{:x}",Sha256::digest(data.as_os_str().to_string_lossy().as_bytes()));Ok(json!({"profile_id":id,"preferences":read(data)}))
 })();Some(match result{Ok(value)=>crate::api_http::json_response("200 OK",&value.to_string()),Err(error)=>crate::api_http::json_response("400 Bad Request",&json!({"error":error.to_string()}).to_string())})
}
#[cfg(test)]mod tests{use super::*;#[test]fn only_presentation_preferences_can_enter_backups(){let value=filtered(&json!({"akasha_locale":"fr","OPENAI_API_KEY":"secret","akasha_session":"private","akasha_theme":"dark"}));assert_eq!(value,json!({"akasha_locale":"fr","akasha_theme":"dark"}));}}
