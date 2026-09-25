import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { InfoTip } from "./Tooltip";

const DAEMON_PORT = 3876;

type EmbeddedStatus = {
  embedded_available?: boolean;
  embedded_loaded?: boolean;
  backend?: string | null;
  device?: string | null;
  model_path?: string | null;
  hint?: string;
  hardware_tier?: string | null;
  active_model_id?: string | null;
  engine_mode?: string | null;
  n_gpu_layers?: number | null;
  active_engine_policy?: string | null;
  calibration_done?: boolean;
  last_bench_tok_per_s?: number | null;
  gguf_present?: boolean;
  llama_cpp_compiled?: boolean;
};

type RuntimeSettings = {
  active_model_id?: string | null;
  engine_mode?: string;
  n_gpu_layers?: number;
  hardware_tier?: string;
  active_engine_policy?: string;
  models?: { id: string; label: string; filename: string }[];
  runtime?: { calibrated_at?: string; winner_tok_per_s?: number | null } | null;
};

type Props = {
  t: (key: string) => string;
  locale: "fr" | "en";
};

function engineModeLabel(mode: string | null | undefined, t: (key: string) => string): string {
  switch (mode) {
    case "cpu":
      return t("settings.embedded_mode_cpu");
    case "cuda":
      return t("settings.embedded_mode_cuda");
    case "auto":
    default:
      return t("settings.embedded_mode_auto");
  }
}

export function EmbeddedLocalModelSettings({ t, locale }: Props) {
  const [status, setStatus] = useState<EmbeddedStatus | null>(null);
  const [runtime, setRuntime] = useState<RuntimeSettings | null>(null);
  const [modelId, setModelId] = useState("");
  const [engineMode, setEngineMode] = useState("auto");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [st, rt] = await Promise.all([
        invoke<EmbeddedStatus>("get_embedded_status", { port: DAEMON_PORT }),
        invoke<RuntimeSettings>("get_embedded_runtime", { port: DAEMON_PORT }),
      ]);
      setStatus(st);
      setRuntime(rt);
      setModelId(rt.active_model_id ?? st.active_model_id ?? rt.models?.[0]?.id ?? "");
      setEngineMode(rt.engine_mode ?? st.engine_mode ?? "auto");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = useCallback(async () => {
    if (!modelId) {
      setError(t("settings.embedded_model_required"));
      return;
    }
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      await invoke("set_embedded_runtime", {
        port: DAEMON_PORT,
        modelId,
        engineMode,
      });
      setMessage(t("settings.embedded_saved"));
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }, [engineMode, load, modelId, t]);

  const reloadModel = useCallback(async () => {
    setSaving(true);
    setError(null);
    try {
      await invoke("embedded_reload", { port: DAEMON_PORT });
      setMessage(t("settings.embedded_reloaded"));
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }, [load, t]);

  if (loading && !status) {
    return <p className="settings-doc muted">{t("common.loading")}</p>;
  }

  const models = runtime?.models ?? [];
  const tier = status?.hardware_tier ?? runtime?.hardware_tier ?? "—";
  const ngl = status?.n_gpu_layers ?? runtime?.n_gpu_layers;
  const policy = status?.active_engine_policy ?? runtime?.active_engine_policy;

  return (
    <section className="settings-card">
      <h4>{t("settings.embedded_title")}</h4>
      <p className="settings-doc muted">{t("settings.embedded_hint")}</p>

      {error ? <p className="steering-queue-error">{error}</p> : null}
      {message ? <p className="settings-doc muted">{message}</p> : null}

      <dl className="settings-list embedded-runtime-status">
        <dt>{t("settings.embedded_tier")}</dt>
        <dd><code>{tier}</code></dd>
        <dt>{t("settings.embedded_active_mode")}</dt>
        <dd>
          <strong>{engineModeLabel(status?.engine_mode ?? runtime?.engine_mode, t)}</strong>
          {ngl != null ? (
            <span className="muted"> — n_gpu_layers={ngl}</span>
          ) : null}
          {policy ? (
            <span className="muted"> ({policy})</span>
          ) : null}
        </dd>
        <dt>{t("settings.embedded_active_model")}</dt>
        <dd><code>{status?.active_model_id ?? runtime?.active_model_id ?? "—"}</code></dd>
        <dt>{t("settings.embedded_device")}</dt>
        <dd>{status?.device ?? "—"}</dd>
        <dt>{t("settings.embedded_backend")}</dt>
        <dd>{status?.backend ?? "—"}</dd>
        {status?.model_path ? (
          <>
            <dt>{t("settings.embedded_gguf_path")}</dt>
            <dd><code className="settings-path">{status.model_path}</code></dd>
          </>
        ) : null}
        {status?.last_bench_tok_per_s != null ? (
          <>
            <dt>{t("settings.embedded_bench")}</dt>
            <dd>{status.last_bench_tok_per_s.toFixed(1)} tok/s</dd>
          </>
        ) : null}
      </dl>

      {status?.hint ? (
        <p className="settings-doc muted embedded-hint">{status.hint}</p>
      ) : null}

      {!status?.embedded_available ? (
        <p className="settings-doc muted">{t("settings.embedded_unavailable")}</p>
      ) : (
        <>
          <div className="settings-field">
            <label htmlFor="embedded-model-select">
              {t("settings.embedded_model_label")}
              <InfoTip content={t("settings.embedded_model_tip")} />
            </label>
            <select
              id="embedded-model-select"
              value={modelId}
              disabled={saving || models.length === 0}
              onChange={(e) => setModelId(e.target.value)}
            >
              {models.length === 0 ? (
                <option value="">{t("settings.embedded_no_models")}</option>
              ) : (
                models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {locale === "fr" ? m.label : m.label}
                  </option>
                ))
              )}
            </select>
          </div>

          <div className="settings-field">
            <label htmlFor="embedded-engine-select">
              {t("settings.embedded_engine_label")}
              <InfoTip content={t("settings.embedded_engine_tip")} />
            </label>
            <select
              id="embedded-engine-select"
              value={engineMode}
              disabled={saving}
              onChange={(e) => setEngineMode(e.target.value)}
            >
              <option value="auto">{t("settings.embedded_mode_auto")}</option>
              <option value="cpu">{t("settings.embedded_mode_cpu")}</option>
              <option value="cuda">{t("settings.embedded_mode_cuda")}</option>
            </select>
          </div>

          <p className="settings-doc muted">{t("settings.embedded_timeout_tip")}</p>

          <div className="settings-row-actions">
            <button
              type="button"
              className="btn-primary"
              disabled={saving || !modelId}
              onClick={() => void save()}
            >
              {saving ? t("common.loading") : t("settings.embedded_save")}
            </button>
            <button
              type="button"
              className="btn-secondary"
              disabled={saving}
              onClick={() => void reloadModel()}
            >
              {t("settings.embedded_reload")}
            </button>
            <button
              type="button"
              className="btn-secondary"
              disabled={loading}
              onClick={() => void load()}
            >
              {t("settings.embedded_refresh")}
            </button>
          </div>
        </>
      )}
    </section>
  );
}
