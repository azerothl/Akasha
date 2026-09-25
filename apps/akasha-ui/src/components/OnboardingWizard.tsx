import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { E2E_WEB, e2eDaemonGetJson } from "../e2eDaemon";

const WIZARD_DONE_KEY = "akasha_setup_wizard_done";
const DAEMON_PORT = 3876;

type Props = {
  locale: "fr" | "en";
  daemonOk: boolean;
  onComplete: () => void;
  t: (key: string) => string;
};

type EmbeddedStatus = {
  embedded_available?: boolean;
  embedded_loaded?: boolean;
  backend?: string | null;
  device?: string | null;
  hint?: string;
  compiled_backends?: string[];
  model_path?: string | null;
  llama_cpp_compiled?: boolean;
  gguf_present?: boolean;
  ready_for_chat?: boolean;
  action?: string | null;
  hardware_tier?: string | null;
  recommended_model_id?: string | null;
  active_engine_policy?: string | null;
  calibration_done?: boolean;
  last_bench_tok_per_s?: number | null;
};

type ManifestModel = {
  id: string;
  label: string;
  label_en?: string;
  label_fr?: string;
  size_bytes_hint?: number;
};

type DownloadProgress = {
  state: string;
  percent?: number;
  error?: string | null;
};

type CalibrateProgress = {
  state: string;
  percent?: number;
  current?: number;
  total?: number;
  current_config?: string | null;
  error?: string | null;
  winner?: {
    model_id: string;
    n_gpu_layers: number;
    backend: string;
    tok_per_s: number;
    device: string;
  } | null;
};

type HardwareInfo = {
  profile?: { tier_id?: string; ram_gb?: number; vram_mb?: number | null; gpu_name?: string | null };
  models_for_tier?: string[];
};

export function readSetupWizardPending(): boolean {
  try {
    return localStorage.getItem(WIZARD_DONE_KEY) !== "1";
  } catch {
    return true;
  }
}

export function markSetupWizardDone(): void {
  try {
    localStorage.setItem(WIZARD_DONE_KEY, "1");
    localStorage.setItem("akasha_onboarding_dismissed", "1");
  } catch {
    /* ignore */
  }
}

export function OnboardingWizard({ locale, daemonOk, onComplete, t }: Props) {
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [doctorMsg, setDoctorMsg] = useState<string | null>(null);
  const [howToCall, setHowToCall] = useState("");
  const [embeddedStatus, setEmbeddedStatus] = useState<EmbeddedStatus | null>(null);
  const [hardwareInfo, setHardwareInfo] = useState<HardwareInfo | null>(null);
  const [models, setModels] = useState<ManifestModel[]>([]);
  const [selectedModelId, setSelectedModelId] = useState<string>("");
  const [downloadProgress, setDownloadProgress] = useState<DownloadProgress | null>(null);
  const [calibrateProgress, setCalibrateProgress] = useState<CalibrateProgress | null>(null);
  const [firstMessageResult, setFirstMessageResult] = useState<string | null>(null);
  const [firstMessageError, setFirstMessageError] = useState<string | null>(null);

  const steps = [
    t("onboarding.step.welcome"),
    t("onboarding.step.health"),
    t("onboarding.step.llm"),
    t("onboarding.step.calibrate"),
    t("onboarding.step.firstMessage"),
    t("onboarding.step.channels"),
    t("onboarding.step.profile"),
    t("onboarding.step.ready"),
  ];

  const loadEmbedded = useCallback(async () => {
    if (!daemonOk) return;
    try {
      const status = E2E_WEB
        ? await e2eDaemonGetJson<EmbeddedStatus>("/api/router/embedded-status")
        : await invoke<EmbeddedStatus>("get_embedded_status", { port: DAEMON_PORT });
      setEmbeddedStatus(status);
      const hw = E2E_WEB
        ? await e2eDaemonGetJson<HardwareInfo>("/api/router/embedded/hardware")
        : await invoke<HardwareInfo>("get_embedded_hardware", { port: DAEMON_PORT });
      setHardwareInfo(hw);
      const manifest = E2E_WEB
        ? await e2eDaemonGetJson<{ models?: ManifestModel[]; default_id?: string }>(
            "/api/router/embedded/models",
          )
        : await invoke<{ models?: ManifestModel[]; default_id?: string }>("get_embedded_models", {
            port: DAEMON_PORT,
          });
      const tierIds = new Set(hw.models_for_tier ?? []);
      const list = (manifest.models ?? []).filter(
        (m) => tierIds.size === 0 || tierIds.has(m.id),
      );
      setModels(list.length > 0 ? list : (manifest.models ?? []));
      const recommended = status.recommended_model_id ?? manifest.default_id ?? list[0]?.id ?? "";
      setSelectedModelId(recommended);
    } catch (e) {
      setDoctorMsg(e instanceof Error ? e.message : String(e));
    }
  }, [daemonOk]);

  useEffect(() => {
    if (step === 2 && daemonOk) {
      void loadEmbedded();
    }
    if (step === 3 && daemonOk) {
      void loadEmbedded();
    }
  }, [step, daemonOk, loadEmbedded]);

  const runDoctorFix = useCallback(async () => {
    setBusy(true);
    setDoctorMsg(null);
    try {
      const out = await invoke<string>("run_akasha_doctor_fix", { port: DAEMON_PORT });
      setDoctorMsg(out || t("onboarding.doctor.done"));
    } catch (e) {
      setDoctorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [t]);

  const startDownload = useCallback(async () => {
    setBusy(true);
    setDownloadProgress({ state: "running", percent: 0 });
    try {
      await invoke("embedded_download_start", {
        port: DAEMON_PORT,
        modelId: selectedModelId || null,
      });
      const poll = window.setInterval(async () => {
        try {
          const prog = await invoke<DownloadProgress>("embedded_download_status", { port: DAEMON_PORT });
          setDownloadProgress(prog);
          if (prog.state === "done" || prog.state === "error") {
            window.clearInterval(poll);
            setBusy(false);
            if (prog.state === "done") void loadEmbedded();
          }
        } catch {
          window.clearInterval(poll);
          setBusy(false);
        }
      }, 500);
    } catch (e) {
      setDownloadProgress({ state: "error", error: e instanceof Error ? e.message : String(e) });
      setBusy(false);
    }
  }, [loadEmbedded, selectedModelId]);

  const startCalibration = useCallback(async () => {
    setBusy(true);
    setCalibrateProgress({ state: "running", percent: 0, current: 0, total: 3 });
    try {
      await invoke("embedded_calibrate_start", { port: DAEMON_PORT, maxConfigs: 3 });
      const poll = window.setInterval(async () => {
        try {
          const prog = await invoke<CalibrateProgress>("embedded_calibrate_status", { port: DAEMON_PORT });
          setCalibrateProgress(prog);
          if (prog.state === "done" || prog.state === "error") {
            window.clearInterval(poll);
            setBusy(false);
            if (prog.state === "done") void loadEmbedded();
          }
        } catch {
          window.clearInterval(poll);
          setBusy(false);
        }
      }, 800);
    } catch (e) {
      setCalibrateProgress({ state: "error", error: e instanceof Error ? e.message : String(e) });
      setBusy(false);
    }
  }, [loadEmbedded]);

  const runFirstMessageTest = useCallback(async () => {
    setBusy(true);
    setFirstMessageError(null);
    setFirstMessageResult(null);
    try {
      const res = await invoke<{ ok?: boolean; reply?: string }>("wizard_test_embedded_message", {
        port: DAEMON_PORT,
      });
      setFirstMessageResult(res.reply ?? t("onboarding.firstMessage.ok"));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg.includes("timeout")) {
        setFirstMessageError(t("onboarding.firstMessage.timeout"));
      } else {
        setFirstMessageError(msg);
      }
    } finally {
      setBusy(false);
    }
  }, [t]);

  const saveProfile = useCallback(async () => {
    if (!howToCall.trim()) return;
    setBusy(true);
    try {
      await invoke("post_user_profile", {
        port: DAEMON_PORT,
        body: {
          how_to_call: howToCall.trim(),
          onboarding_completed: true,
        },
      });
      markSetupWizardDone();
      onComplete();
    } catch {
      setDoctorMsg(t("onboarding.profile.error"));
    } finally {
      setBusy(false);
    }
  }, [howToCall, onComplete, t]);

  const modelLabel = (m: ManifestModel) =>
    locale === "fr" ? m.label_fr ?? m.label : m.label_en ?? m.label;

  const needsDownload =
    embeddedStatus?.llama_cpp_compiled &&
    !embeddedStatus?.gguf_present &&
    embeddedStatus?.action === "embedded-download";

  const needsCalibrate =
    embeddedStatus?.gguf_present &&
    embeddedStatus?.llama_cpp_compiled &&
    !embeddedStatus?.calibration_done;

  const showLoadingHint =
    embeddedStatus && !embeddedStatus.embedded_loaded && embeddedStatus.ready_for_chat;

  const canAdvanceFromCalibrate = embeddedStatus?.calibration_done === true;

  const calibrateBlocked = step === 3 && needsCalibrate && !canAdvanceFromCalibrate;

  return (
    <div className="human-input-overlay onboarding-overlay" role="dialog" aria-modal="true">
      <div className="human-input-modal onboarding-wizard-modal">
        <h2>{t("onboarding.title")}</h2>
        <p className="onboarding-wizard-steps-label">
          {steps[step]} ({step + 1}/{steps.length})
        </p>
        {step === 0 && (
          <>
            <p>{t("onboarding.intro")}</p>
            <p className="muted">
              {daemonOk ? t("onboarding.daemon.ok") : t("onboarding.daemon.off")}
            </p>
          </>
        )}
        {step === 1 && (
          <>
            <p>{t("onboarding.health.hint")}</p>
            <button type="button" className="btn-primary" disabled={busy || !daemonOk} onClick={() => void runDoctorFix()}>
              {busy ? "…" : t("onboarding.health.runFix")}
            </button>
            {doctorMsg ? <pre className="onboarding-doctor-output">{doctorMsg}</pre> : null}
          </>
        )}
        {step === 2 && (
          <>
            {embeddedStatus ? (
              <div className="onboarding-embedded-status">
                <p>
                  <strong>{t("onboarding.embedded.backend")}:</strong>{" "}
                  {embeddedStatus.backend ?? "—"} ({embeddedStatus.device ?? "—"})
                </p>
                {hardwareInfo?.profile?.tier_id ? (
                  <p className="muted">
                    {t("onboarding.embedded.tier")}: {hardwareInfo.profile.tier_id}
                    {hardwareInfo.profile.gpu_name ? ` — ${hardwareInfo.profile.gpu_name}` : ""}
                  </p>
                ) : null}
                <p className="muted">{embeddedStatus.hint}</p>
                {embeddedStatus.compiled_backends?.length ? (
                  <p className="muted">
                    {t("onboarding.embedded.compiled")}: {embeddedStatus.compiled_backends.join(", ")}
                  </p>
                ) : null}
                {showLoadingHint ? (
                  <p className="onboarding-loading-hint">{t("onboarding.embedded.loadingHint")}</p>
                ) : null}
                {models.length > 0 ? (
                  <label htmlFor="wizard-model-pick">
                    {t("onboarding.embedded.pickModel")}
                    <select
                      id="wizard-model-pick"
                      className="onboarding-wizard-input"
                      value={selectedModelId}
                      onChange={(e) => setSelectedModelId(e.target.value)}
                    >
                      {models.map((m) => (
                        <option key={m.id} value={m.id}>
                          {modelLabel(m)}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : null}
                {needsDownload ? (
                  <>
                    <button type="button" className="btn-primary" disabled={busy || !daemonOk} onClick={() => void startDownload()}>
                      {busy ? "…" : t("onboarding.embedded.download")}
                    </button>
                    {downloadProgress ? (
                      <p className="muted">
                        {downloadProgress.state === "running"
                          ? `${t("onboarding.embedded.downloading")} ${Math.round(downloadProgress.percent ?? 0)}%`
                          : downloadProgress.state === "error"
                            ? downloadProgress.error
                            : downloadProgress.state === "done"
                              ? t("onboarding.embedded.downloadDone")
                              : null}
                      </p>
                    ) : null}
                  </>
                ) : embeddedStatus.ready_for_chat ? (
                  <p>{t("onboarding.embedded.ready")}</p>
                ) : null}
              </div>
            ) : (
              <p className="muted">{t("onboarding.embedded.loading")}</p>
            )}
          </>
        )}
        {step === 3 && (
          <>
            <p>{t("onboarding.calibrate.hint")}</p>
            <p className="onboarding-loading-hint">{t("onboarding.calibrate.duration")}</p>
            {embeddedStatus?.calibration_done && embeddedStatus.last_bench_tok_per_s != null ? (
              <p>
                {t("onboarding.calibrate.done")}: {embeddedStatus.recommended_model_id ?? "—"},{" "}
                {embeddedStatus.active_engine_policy ?? "—"}, ~{embeddedStatus.last_bench_tok_per_s.toFixed(1)}{" "}
                tok/s
              </p>
            ) : needsCalibrate ? (
              <button
                type="button"
                className="btn-primary"
                disabled={busy || !daemonOk}
                onClick={() => void startCalibration()}
              >
                {busy ? t("onboarding.calibrate.running") : t("onboarding.calibrate.run")}
              </button>
            ) : (
              <p className="muted">{t("onboarding.calibrate.skipHint")}</p>
            )}
            {calibrateProgress ? (
              <p className="muted">
                {calibrateProgress.state === "running"
                  ? `${t("onboarding.calibrate.progress")} ${calibrateProgress.current ?? 0}/${calibrateProgress.total ?? 3}${
                      calibrateProgress.current_config ? ` — ${calibrateProgress.current_config}` : ""
                    } (${Math.round(calibrateProgress.percent ?? 0)}%)`
                  : calibrateProgress.state === "error"
                    ? calibrateProgress.error
                    : calibrateProgress.state === "done" && calibrateProgress.winner
                      ? `${t("onboarding.calibrate.winner")}: ${calibrateProgress.winner.model_id}, ${
                          calibrateProgress.winner.backend
                        }, ~${calibrateProgress.winner.tok_per_s.toFixed(1)} tok/s`
                      : null}
              </p>
            ) : null}
          </>
        )}
        {step === 4 && (
          <>
            <p>{t("onboarding.firstMessage.hint")}</p>
            <p className="onboarding-loading-hint">{t("onboarding.embedded.loadingHint")}</p>
            <button type="button" className="btn-primary" disabled={busy || !daemonOk} onClick={() => void runFirstMessageTest()}>
              {busy ? t("onboarding.firstMessage.running") : t("onboarding.firstMessage.run")}
            </button>
            {firstMessageResult ? (
              <pre className="onboarding-doctor-output">{firstMessageResult}</pre>
            ) : null}
            {firstMessageError ? <p className="onboarding-error">{firstMessageError}</p> : null}
          </>
        )}
        {step === 5 && (
          <>
            <p>{t("onboarding.channels.hint")}</p>
            <p className="muted">{t("onboarding.channels.detail")}</p>
          </>
        )}
        {step === 6 && (
          <>
            <label htmlFor="wizard-how-to-call">{t("onboarding.profile.label")}</label>
            <input
              id="wizard-how-to-call"
              className="onboarding-wizard-input"
              value={howToCall}
              onChange={(e) => setHowToCall(e.target.value)}
              placeholder={t("onboarding.profile.placeholder")}
            />
          </>
        )}
        {step === 7 && <p>{t("onboarding.ready")}</p>}
        <div className="onboarding-actions onboarding-wizard-actions">
          {step > 0 ? (
            <button type="button" className="btn-secondary" onClick={() => setStep((s) => s - 1)}>
              {t("onboarding.back")}
            </button>
          ) : null}
          {step < steps.length - 1 ? (
            <button
              type="button"
              className="btn-primary"
              disabled={(step === 1 && !daemonOk) || calibrateBlocked}
              onClick={() => setStep((s) => s + 1)}
            >
              {t("onboarding.next")}
            </button>
          ) : (
            <button
              type="button"
              className="btn-primary"
              onClick={() => {
                if (howToCall.trim()) void saveProfile();
                else {
                  markSetupWizardDone();
                  onComplete();
                }
              }}
            >
              {t("onboarding.finish")}
            </button>
          )}
          <button
            type="button"
            className="onboarding-dismiss"
            onClick={() => {
              markSetupWizardDone();
              onComplete();
            }}
          >
            {t("onboarding.dismiss")}
          </button>
        </div>
      </div>
    </div>
  );
}
