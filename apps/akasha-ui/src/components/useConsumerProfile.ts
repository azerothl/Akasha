import {useEffect} from "react";
import type {PrivacyFetch} from "./RemotePrivacy";
export const PROFILE_PREFERENCE_KEYS=["akasha_locale","akasha_theme","akasha_ui_mode","akasha_ui_density","akasha_ui_chat_tips","akasha_ui_prompt_chips","akasha_ui_buddy","akasha_chat_dual_pane","akasha_companion_open","akasha_setup_wizard_done","akasha_onboarding_dismissed"];
export function useConsumerProfile(fetchEndpoint:PrivacyFetch,ready:boolean){
 useEffect(()=>{if(!ready)return;let disposed=false;
  void fetchEndpoint("/api/consumer/preferences").then(response=>{if(!response.ok||disposed)return;const value=JSON.parse(response.text);if(typeof value.profile_id!=="string")return;const previous=localStorage.getItem("akasha_active_profile_id");if(previous===value.profile_id)return;localStorage.setItem("akasha_active_profile_id",value.profile_id);
   let changed=!!previous;
   if(previous){for(const key of Object.keys(localStorage)){if(["akasha_session_id","akasha_chat_threads_v1","akasha_task_sessions_v1"].includes(key)||/^(akasha_map_|akasha_chat_draft|akasha_document_|akasha_consumer_job)/.test(key))localStorage.removeItem(key);}}
   for(const key of PROFILE_PREFERENCE_KEYS){const next=value.preferences?.[key];if(typeof next==="string"&&localStorage.getItem(key)!==next){localStorage.setItem(key,next);changed=true;}}
   if(changed)window.location.reload();
  }).catch(()=>{});return()=>{disposed=true;};
 },[fetchEndpoint,ready]);
}
