import { useNotifyOnMessage } from "../notifications/useNotifyOnMessage";
import { useI18n } from "../useI18n";

type Props = {
  routerError: string | null;
  docError: string | null;
  memoryError: string | null;
  missionError: string | null;
  calendarTaskDetailError: string | null;
  scheduleDetailError: string | null;
  pluginStatusError: string | null;
  pluginReputationResetError: string | null;
  agentProfileError: string | null;
  userProfileError: string | null;
  userRagError: string | null;
  projectGraphError: string | null;
};

/** Syncs panel error state into the header notification center. */
export function AppNotificationsSync({
  routerError,
  docError,
  memoryError,
  missionError,
  calendarTaskDetailError,
  scheduleDetailError,
  pluginStatusError,
  pluginReputationResetError,
  agentProfileError,
  userProfileError,
  userRagError,
  projectGraphError,
}: Props) {
  const { t } = useI18n();

  useNotifyOnMessage(routerError, "error", t("tabs.router"));
  useNotifyOnMessage(docError, "error", t("tabs.docs"));
  useNotifyOnMessage(memoryError, "error", t("tabs.memory"));
  useNotifyOnMessage(
    missionError === "unavailable" ? t("mission.unavailable") : missionError,
    missionError === "unavailable" ? "warning" : "error",
    t("tabs.mission"),
  );
  useNotifyOnMessage(calendarTaskDetailError, "error", t("tabs.calendar"));
  useNotifyOnMessage(scheduleDetailError, "error", t("tabs.scheduled"));
  useNotifyOnMessage(pluginStatusError, "error", t("settings.plugins_status_title"));
  useNotifyOnMessage(pluginReputationResetError, "error", t("settings.plugin_reputation_reset_one"));
  useNotifyOnMessage(agentProfileError, "error", t("settings.agent_profile_title"));
  useNotifyOnMessage(userProfileError, "error", t("settings.user_profile_first_name"));
  useNotifyOnMessage(userRagError, "error", t("settings.user_rag_title"));
  useNotifyOnMessage(projectGraphError, "error", t("settings.project_graph_list_title"));

  return null;
}
