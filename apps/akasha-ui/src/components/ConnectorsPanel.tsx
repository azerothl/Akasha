import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { InfoTip } from "./Tooltip";

const DAEMON_PORT = 3876;

type ConnectorRow = {
  id: string;
  env_key: string;
  enabled_in_file: boolean;
  active_in_process: boolean;
};

type ConnectorsConfigView = {
  telegram_notify_chat_id?: string | null;
  telegram_token_configured?: boolean;
  slack_signing_secret_configured?: boolean;
  discord_token_configured?: boolean;
};

type Props = {
  t: (key: string) => string;
};

function SecretInput({
  id,
  label,
  hint,
  configured,
  value,
  onChange,
  placeholder,
}: {
  id: string;
  label: string;
  hint: string;
  configured: boolean;
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
}) {
  return (
    <label className="connectors-secret-field" htmlFor={id}>
      <span className="connectors-field-label">
        {label}
        <InfoTip label={label} content={hint} />
      </span>
      <input
        id={id}
        type="password"
        className="settings-input"
        value={value}
        autoComplete="off"
        placeholder={configured ? placeholder : ""}
        onChange={(e) => onChange(e.target.value)}
      />
      {configured ? (
        <span className="settings-doc muted">{placeholder}</span>
      ) : null}
    </label>
  );
}

export function ConnectorsPanel({ t }: Props) {
  const [connectors, setConnectors] = useState<ConnectorRow[]>([]);
  const [config, setConfig] = useState<ConnectorsConfigView>({});
  const [telegram, setTelegram] = useState(false);
  const [slack, setSlack] = useState(false);
  const [discord, setDiscord] = useState(false);
  const [telegramNotifyChatId, setTelegramNotifyChatId] = useState("");
  const [telegramToken, setTelegramToken] = useState("");
  const [slackSecret, setSlackSecret] = useState("");
  const [discordToken, setDiscordToken] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [restartRequired, setRestartRequired] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setMsg(null);
    try {
      const json = await invoke<{
        connectors?: ConnectorRow[];
        config?: ConnectorsConfigView;
        restart_required?: boolean;
      }>("get_connectors", { port: DAEMON_PORT });
      const rows = Array.isArray(json?.connectors) ? json.connectors : [];
      const cfg = json?.config ?? {};
      setConnectors(rows);
      setConfig(cfg);
      setRestartRequired(Boolean(json?.restart_required));
      setTelegram(rows.find((c) => c.id === "telegram")?.enabled_in_file ?? false);
      setSlack(rows.find((c) => c.id === "slack")?.enabled_in_file ?? false);
      setDiscord(rows.find((c) => c.id === "discord")?.enabled_in_file ?? false);
      setTelegramNotifyChatId(cfg.telegram_notify_chat_id ?? "");
      setTelegramToken("");
      setSlackSecret("");
      setDiscordToken("");
    } catch (e) {
      setMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    setSaving(true);
    setMsg(null);
    try {
      const body: Record<string, string | boolean> = {
        telegram,
        slack,
        discord,
        telegram_notify_chat_id: telegramNotifyChatId.trim(),
      };
      if (telegramToken.trim()) body.telegram_bot_token = telegramToken.trim();
      if (slackSecret.trim()) body.slack_signing_secret = slackSecret.trim();
      if (discordToken.trim()) body.discord_bot_token = discordToken.trim();
      await invoke("post_connectors", { body, port: DAEMON_PORT });
      setMsg(t("connectors.saved"));
      setRestartRequired(true);
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const restart = async () => {
    setRestarting(true);
    setMsg(null);
    try {
      await invoke("restart_daemon", { port: DAEMON_PORT });
      setMsg(t("connectors.restart_sent"));
      setRestartRequired(false);
    } catch (e) {
      setMsg(String(e));
    } finally {
      setRestarting(false);
    }
  };

  const statusLabel = (row: ConnectorRow | undefined, enabled: boolean) => {
    if (!row) return null;
    if (row.active_in_process && enabled) return t("connectors.status_active");
    if (enabled && !row.active_in_process) return t("connectors.status_pending_restart");
    return t("connectors.status_inactive");
  };

  if (loading) {
    return <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>;
  }

  return (
    <div className="connectors-panel">
      <p className="settings-doc muted">{t("connectors.desc")}</p>

      <details className="connectors-section" open={telegram}>
        <summary className="connectors-summary">
          Telegram
          <InfoTip label="Telegram" content={t("connectors.telegram_section_help")} />
        </summary>
        <p className="settings-doc muted connectors-section-help">{t("connectors.telegram_section_help")}</p>
        <div className="connectors-section-body">
          <label className="settings-checkbox-label">
            <input type="checkbox" checked={telegram} onChange={(e) => setTelegram(e.target.checked)} />
            {t("connectors.enable")}
          </label>
          <span className="settings-doc muted">{statusLabel(connectors.find((c) => c.id === "telegram"), telegram)}</span>
          <SecretInput
            id="telegram-bot-token"
            label={t("connectors.telegram_token")}
            hint={t("connectors.telegram_token_hint")}
            configured={Boolean(config.telegram_token_configured)}
            value={telegramToken}
            onChange={setTelegramToken}
            placeholder={t("connectors.secret_keep_placeholder")}
          />
          <label className="connectors-field" htmlFor="telegram-notify-chat-id">
            <span className="connectors-field-label">
              {t("connectors.telegram_notify_chat_id")}
              <InfoTip label={t("connectors.telegram_notify_chat_id")} content={t("connectors.telegram_notify_chat_id_hint")} />
            </span>
            <input
              id="telegram-notify-chat-id"
              type="text"
              className="settings-input"
              value={telegramNotifyChatId}
              placeholder="123456789"
              onChange={(e) => setTelegramNotifyChatId(e.target.value)}
            />
          </label>
        </div>
      </details>

      <details className="connectors-section" open={slack}>
        <summary className="connectors-summary">
          Slack
          <InfoTip label="Slack" content={t("connectors.slack_section_help")} />
        </summary>
        <p className="settings-doc muted connectors-section-help">{t("connectors.slack_section_help")}</p>
        <div className="connectors-section-body">
          <label className="settings-checkbox-label">
            <input type="checkbox" checked={slack} onChange={(e) => setSlack(e.target.checked)} />
            {t("connectors.enable")}
          </label>
          <span className="settings-doc muted">{statusLabel(connectors.find((c) => c.id === "slack"), slack)}</span>
          <SecretInput
            id="slack-signing-secret"
            label={t("connectors.slack_signing_secret")}
            hint={t("connectors.slack_signing_secret_hint")}
            configured={Boolean(config.slack_signing_secret_configured)}
            value={slackSecret}
            onChange={setSlackSecret}
            placeholder={t("connectors.secret_keep_placeholder")}
          />
        </div>
      </details>

      <details className="connectors-section" open={discord}>
        <summary className="connectors-summary">
          Discord
          <InfoTip label="Discord" content={t("connectors.discord_section_help")} />
        </summary>
        <p className="settings-doc muted connectors-section-help">{t("connectors.discord_section_help")}</p>
        <div className="connectors-section-body">
          <label className="settings-checkbox-label">
            <input type="checkbox" checked={discord} onChange={(e) => setDiscord(e.target.checked)} />
            {t("connectors.enable")}
          </label>
          <span className="settings-doc muted">{statusLabel(connectors.find((c) => c.id === "discord"), discord)}</span>
          <SecretInput
            id="discord-bot-token"
            label={t("connectors.discord_token")}
            hint={t("connectors.discord_token_hint")}
            configured={Boolean(config.discord_token_configured)}
            value={discordToken}
            onChange={setDiscordToken}
            placeholder={t("connectors.secret_keep_placeholder")}
          />
        </div>
      </details>

      {restartRequired ? (
        <p className="settings-doc connectors-restart-hint">{t("connectors.restart_required")}</p>
      ) : null}

      <div className="settings-row-actions">
        <button type="button" className="btn-primary" disabled={saving} onClick={() => void save()}>
          {saving ? t("common.loading") : t("connectors.save")}
        </button>
        <button
          type="button"
          className="btn-secondary"
          disabled={restarting}
          onClick={() => void restart()}
        >
          {restarting ? t("common.loading") : t("connectors.restart_daemon")}
        </button>
      </div>

      <section className="settings-card connectors-matrix-note">
        <h4>
          Matrix
          <InfoTip label="Matrix" content={t("connectors.matrix_desc")} />
        </h4>
        <p className="settings-doc muted">{t("connectors.matrix_desc")}</p>
      </section>

      {msg ? <p className="muted">{msg}</p> : null}
    </div>
  );
}
