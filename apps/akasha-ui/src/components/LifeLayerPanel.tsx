import { useCallback, useEffect, useState } from "react";
import { InfoTip } from "./Tooltip";

type LifePack = {
  id: string;
  enabled: boolean;
  schedule_id?: string | null;
  hour_local: number;
  minute_local: number;
  timezone: string;
  notify_channel?: string | null;
};

type NlPreview = {
  name: string;
  description: string;
  rrule: string;
  timezone: string;
  hour_local: number;
  minute_local: number;
  tag?: string | null;
  notify_channel?: string | null;
  message: string;
  confidence: number;
};

type Props = {
  locale: "fr" | "en";
  t: (key: string) => string;
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
};

export function LifeLayerPanel({ locale, t, fetchEndpoint }: Props) {
  const [packs, setPacks] = useState<LifePack[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [nlText, setNlText] = useState("");
  const [nlPreview, setNlPreview] = useState<NlPreview | null>(null);
  const [morningHour, setMorningHour] = useState(8);
  const [morningMinute, setMorningMinute] = useState(0);
  const [overnightHour, setOvernightHour] = useState(2);
  const [overnightMinute, setOvernightMinute] = useState(0);
  const [notifyChannel, setNotifyChannel] = useState("telegram");
  const [cronNameContains, setCronNameContains] = useState("");
  const [cronOnFailure, setCronOnFailure] = useState(true);
  const [cronMessage, setCronMessage] = useState("");
  const [cronSubs, setCronSubs] = useState<Array<{ id: string; name_contains?: string; on_failure?: boolean; message?: string }>>([]);

  const load = useCallback(async () => {
    setLoading(true);
    setMsg(null);
    try {
      const res = await fetchEndpoint("/api/life/packs");
      if (!res.ok) throw new Error(res.text || String(res.status));
      const json = JSON.parse(res.text) as { packs?: LifePack[] };
      const list = json.packs ?? [];
      setPacks(list);
      const morning = list.find((p) => p.id === "morning_brief");
      const overnight = list.find((p) => p.id === "overnight_pack");
      if (morning) {
        setMorningHour(morning.hour_local);
        setMorningMinute(morning.minute_local);
        if (morning.notify_channel) setNotifyChannel(morning.notify_channel);
      }
      if (overnight) {
        setOvernightHour(overnight.hour_local);
        setOvernightMinute(overnight.minute_local);
      }
      const cw = await fetchEndpoint("/api/schedules/watch/subscriptions");
      if (cw.ok) {
        const cj = JSON.parse(cw.text) as {
          subscriptions?: Array<{ id: string; name_contains?: string; on_failure?: boolean; message?: string }>;
        };
        setCronSubs(cj.subscriptions ?? []);
      }
    } catch (e) {
      setMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, [fetchEndpoint]);

  useEffect(() => {
    void load();
  }, [load]);

  const postJson = async (path: string, body: unknown) => {
    const res = await fetchEndpoint(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      try {
        const err = JSON.parse(res.text) as { detail?: string; error?: string };
        throw new Error(err.detail || err.error || res.text);
      } catch (e) {
        if (e instanceof Error && e.message && !e.message.includes("JSON")) throw e;
        throw new Error(res.text || String(res.status));
      }
    }
    return JSON.parse(res.text) as {
      ok?: boolean;
      pack?: LifePack;
      preview?: NlPreview;
      committed?: boolean;
    };
  };

  const saveMorning = async (enabled: boolean) => {
    setBusy("morning");
    setMsg(null);
    try {
      await postJson("/api/life/morning-brief", {
        enabled,
        hour_local: morningHour,
        minute_local: morningMinute,
        timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
        notify_channel: notifyChannel,
      });
      setMsg(locale === "en" ? "Morning brief saved." : "Brief matinal enregistré.");
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const saveOvernight = async (enabled: boolean) => {
    setBusy("overnight");
    setMsg(null);
    try {
      await postJson("/api/life/overnight-pack", {
        enabled,
        hour_local: overnightHour,
        minute_local: overnightMinute,
        timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
      });
      setMsg(locale === "en" ? "Overnight pack saved." : "Pack nuit enregistré.");
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const previewNl = async () => {
    setBusy("nl");
    setMsg(null);
    try {
      const res = await postJson("/api/schedules/from-nl", { text: nlText, commit: false });
      setNlPreview(res.preview ?? null);
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const commitNl = async () => {
    setBusy("nl");
    setMsg(null);
    try {
      const res = await postJson("/api/schedules/from-nl", { text: nlText, commit: true });
      setNlPreview(res.preview ?? null);
      setMsg(locale === "en" ? "Schedule created." : "Schedule créé.");
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const addCronWatch = async () => {
    setBusy("cron");
    setMsg(null);
    try {
      await postJson("/api/schedules/watch/subscriptions", {
        name_contains: cronNameContains.trim(),
        on_failure: cronOnFailure,
        message:
          cronMessage.trim() ||
          (locale === "en" ? "Schedule exit matched" : "Condition schedule atteinte"),
      });
      setCronMessage("");
      await load();
      setMsg(locale === "en" ? "Cron watch subscription added." : "Abonnement cron watch ajouté.");
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const removeCronWatch = async (id: string) => {
    setBusy("cron");
    setMsg(null);
    try {
      const res = await fetchEndpoint(`/api/schedules/watch/subscriptions/${encodeURIComponent(id)}`, {
        method: "DELETE",
      });
      if (!res.ok) throw new Error(res.text || String(res.status));
      await load();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setBusy(null);
    }
  };

  const morning = packs.find((p) => p.id === "morning_brief");
  const overnight = packs.find((p) => p.id === "overnight_pack");

  return (
    <div className="life-layer-panel">
      <h3>
        {locale === "en" ? "Life layer" : "Life layer"}
        <InfoTip
          label="Life layer"
          content={
            locale === "en"
              ? "Overnight pack, morning brief to Telegram, and natural-language schedules (Hermes-inspired)."
              : "Pack nuit, brief matinal Telegram, et schedules en langage naturel (inspiré Hermes)."
          }
        />
      </h3>
      {loading ? <p className="muted">{t("common.loading")}</p> : null}
      {msg ? <p className="settings-doc">{msg}</p> : null}

      <section className="life-layer-card">
        <h4>{locale === "en" ? "Morning brief" : "Brief matinal"}</h4>
        <p className="muted settings-doc">
          {locale === "en"
            ? "Daily digest pushed to your notify channel after the schedule runs."
            : "Digest quotidien poussé sur votre canal notify après exécution."}
        </p>
        <div className="life-layer-row">
          <label>
            {locale === "en" ? "Hour" : "Heure"}
            <input
              type="number"
              min={0}
              max={23}
              className="settings-input"
              value={morningHour}
              onChange={(e) => setMorningHour(Number(e.target.value))}
            />
          </label>
          <label>
            {locale === "en" ? "Minute" : "Minute"}
            <input
              type="number"
              min={0}
              max={59}
              className="settings-input"
              value={morningMinute}
              onChange={(e) => setMorningMinute(Number(e.target.value))}
            />
          </label>
          <label>
            {locale === "en" ? "Channel" : "Canal"}
            <select
              className="settings-theme-select"
              value={notifyChannel}
              onChange={(e) => setNotifyChannel(e.target.value)}
            >
              <option value="telegram">Telegram</option>
            </select>
          </label>
        </div>
        <div className="life-layer-actions">
          <button type="button" className="btn-primary btn-tiny" disabled={busy === "morning"} onClick={() => void saveMorning(true)}>
            {morning?.enabled
              ? locale === "en"
                ? "Update & enable"
                : "Mettre à jour & activer"
              : locale === "en"
                ? "Enable"
                : "Activer"}
          </button>
          {morning?.enabled ? (
            <button type="button" className="btn-secondary btn-tiny" disabled={busy === "morning"} onClick={() => void saveMorning(false)}>
              {locale === "en" ? "Pause" : "Pause"}
            </button>
          ) : null}
        </div>
      </section>

      <section className="life-layer-card">
        <h4>{locale === "en" ? "Overnight pack" : "Pack nuit"}</h4>
        <p className="muted settings-doc">
          {locale === "en"
            ? "Nightly agent pass with a Markdown report (no email send)."
            : "Passage nocturne avec rapport Markdown (pas d’envoi d’email)."}
        </p>
        <div className="life-layer-row">
          <label>
            {locale === "en" ? "Hour" : "Heure"}
            <input
              type="number"
              min={0}
              max={23}
              className="settings-input"
              value={overnightHour}
              onChange={(e) => setOvernightHour(Number(e.target.value))}
            />
          </label>
          <label>
            {locale === "en" ? "Minute" : "Minute"}
            <input
              type="number"
              min={0}
              max={59}
              className="settings-input"
              value={overnightMinute}
              onChange={(e) => setOvernightMinute(Number(e.target.value))}
            />
          </label>
        </div>
        <div className="life-layer-actions">
          <button type="button" className="btn-primary btn-tiny" disabled={busy === "overnight"} onClick={() => void saveOvernight(true)}>
            {overnight?.enabled
              ? locale === "en"
                ? "Update & enable"
                : "Mettre à jour & activer"
              : locale === "en"
                ? "Enable"
                : "Activer"}
          </button>
          {overnight?.enabled ? (
            <button type="button" className="btn-secondary btn-tiny" disabled={busy === "overnight"} onClick={() => void saveOvernight(false)}>
              {locale === "en" ? "Pause" : "Pause"}
            </button>
          ) : null}
        </div>
      </section>

      <section className="life-layer-card">
        <h4>{locale === "en" ? "Schedule in natural language" : "Planifier en langage naturel"}</h4>
        <p className="muted settings-doc">
          {locale === "en"
            ? 'Example: "chaque matin à 7h30, brief Telegram"'
            : "Exemple : « chaque matin à 7h30, brief Telegram »"}
        </p>
        <textarea
          className="settings-input"
          rows={3}
          value={nlText}
          onChange={(e) => setNlText(e.target.value)}
          placeholder={locale === "en" ? "Describe the schedule…" : "Décrivez le schedule…"}
        />
        <div className="life-layer-actions">
          <button type="button" className="btn-secondary btn-tiny" disabled={!nlText.trim() || busy === "nl"} onClick={() => void previewNl()}>
            {locale === "en" ? "Preview" : "Aperçu"}
          </button>
          <button type="button" className="btn-primary btn-tiny" disabled={!nlText.trim() || busy === "nl"} onClick={() => void commitNl()}>
            {locale === "en" ? "Create" : "Créer"}
          </button>
        </div>
        {nlPreview ? (
          <pre className="life-layer-preview">
            {JSON.stringify(
              {
                name: nlPreview.name,
                rrule: nlPreview.rrule,
                hour: nlPreview.hour_local,
                minute: nlPreview.minute_local,
                notify: nlPreview.notify_channel,
                confidence: nlPreview.confidence,
              },
              null,
              2,
            )}
          </pre>
        ) : null}
      </section>

      <section className="life-layer-card">
        <h4>{locale === "en" ? "Cron watch → wakeup" : "Cron watch → wakeup"}</h4>
        <p className="muted settings-doc">
          {locale === "en"
            ? "When a matching schedule run exits (optionally only on failure), enqueue an agent wakeup."
            : "Quand un schedule correspondant se termine (optionnellement seulement en échec), créer un wakeup agent."}
        </p>
        <div className="life-layer-row">
          <label>
            {locale === "en" ? "Name contains" : "Nom contient"}
            <input
              className="settings-input"
              value={cronNameContains}
              onChange={(e) => setCronNameContains(e.target.value)}
              placeholder="overnight"
            />
          </label>
          <label className="life-layer-check">
            <input
              type="checkbox"
              checked={cronOnFailure}
              onChange={(e) => setCronOnFailure(e.target.checked)}
            />
            {locale === "en" ? "On failure only" : "Échec seulement"}
          </label>
        </div>
        <label>
          {locale === "en" ? "Wakeup message" : "Message wakeup"}
          <input
            className="settings-input"
            value={cronMessage}
            onChange={(e) => setCronMessage(e.target.value)}
          />
        </label>
        <div className="life-layer-actions">
          <button
            type="button"
            className="btn-primary btn-tiny"
            disabled={busy === "cron"}
            onClick={() => void addCronWatch()}
          >
            {locale === "en" ? "Add watch" : "Ajouter watch"}
          </button>
        </div>
        {cronSubs.length > 0 ? (
          <ul className="life-layer-cron-subs">
            {cronSubs.map((s) => (
              <li key={s.id}>
                <span>
                  {s.name_contains || "*"}
                  {s.on_failure ? " · fail" : ""}
                  {s.message ? ` — ${s.message}` : ""}
                </span>
                <button
                  type="button"
                  className="btn-secondary btn-tiny"
                  disabled={busy === "cron"}
                  onClick={() => void removeCronWatch(s.id)}
                >
                  {locale === "en" ? "Remove" : "Retirer"}
                </button>
              </li>
            ))}
          </ul>
        ) : null}
      </section>
    </div>
  );
}
