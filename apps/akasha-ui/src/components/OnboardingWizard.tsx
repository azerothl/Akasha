import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const WIZARD_DONE_KEY = "akasha_setup_wizard_done";
const DAEMON_PORT = 3876;

type Props = {
  locale: "fr" | "en";
  daemonOk: boolean;
  onComplete: () => void;
  t: (key: string) => string;
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

  const runDoctorFix = useCallback(async () => {
    setBusy(true);
    setDoctorMsg(null);
    try {
      const out = await invoke<string>("run_akasha_doctor_fix", { port: DAEMON_PORT });
      setDoctorMsg(out || (locale === "en" ? "Doctor fix completed." : "Doctor fix terminé."));
    } catch (e) {
      setDoctorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [locale]);

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
      setDoctorMsg(locale === "en" ? "Could not save profile." : "Impossible d'enregistrer le profil.");
    } finally {
      setBusy(false);
    }
  }, [howToCall, locale, onComplete]);

  const steps =
    locale === "en"
      ? ["Welcome", "Health check", "LLM provider", "Channels", "Your name", "Ready"]
      : ["Bienvenue", "Diagnostic", "Provider LLM", "Canaux", "Votre prénom", "Prêt"];

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
              {daemonOk
                ? locale === "en"
                  ? "Daemon is connected."
                  : "Le daemon est connecté."
                : locale === "en"
                  ? "Start the daemon with `akasha start` before continuing."
                  : "Démarrez le daemon avec `akasha start` avant de continuer."}
            </p>
          </>
        )}
        {step === 1 && (
          <>
            <p>
              {locale === "en"
                ? "Run doctor --fix to create missing config files (llm_router.yaml, tools_policy, vault)."
                : "Lancez doctor --fix pour créer les fichiers de config manquants (llm_router.yaml, tools_policy, vault)."}
            </p>
            <button type="button" className="btn-primary" disabled={busy || !daemonOk} onClick={() => void runDoctorFix()}>
              {busy ? "…" : locale === "en" ? "Run doctor --fix" : "Lancer doctor --fix"}
            </button>
            {doctorMsg ? <pre className="onboarding-doctor-output">{doctorMsg}</pre> : null}
          </>
        )}
        {step === 2 && (
          <>
            <p>
              {locale === "en"
                ? "Choose your LLM provider in Settings > System after setup: Ollama (local), OpenAI, OpenRouter, or the embedded model."
                : "Choisissez votre provider LLM dans Réglages > Système après la configuration : Ollama (local), OpenAI, OpenRouter, ou le modèle embarqué."}
            </p>
            <p className="muted">
              {locale === "en"
                ? "Edit ~/akasha/llm_router.yaml or use the UI provider picker. External APIs need keys in the vault."
                : "Éditez ~/akasha/llm_router.yaml ou utilisez le sélecteur UI. Les APIs externes nécessitent des clés dans le vault."}
            </p>
          </>
        )}
        {step === 3 && (
          <>
            <p>
              {locale === "en"
                ? "Optional messaging channels: Telegram, Slack, Discord, Teams, and Matrix (sidecar plugin)."
                : "Canaux de messagerie optionnels : Telegram, Slack, Discord, Teams et Matrix (plugin sidecar)."}
            </p>
            <p className="muted">
              {locale === "en"
                ? "Configure connectors in Settings → System → Connectors (or connectors.env). Matrix requires MATRIX_HOMESERVER_URL + access token and the matrix-channel sidecar."
                : "Configurez les connecteurs dans Paramètres → Système → Connecteurs (ou connectors.env). Matrix nécessite MATRIX_HOMESERVER_URL + token et le sidecar matrix-channel."}
            </p>
          </>
        )}
        {step === 4 && (
          <>
            <label htmlFor="wizard-how-to-call">
              {locale === "en" ? "How should Akasha call you?" : "Comment Akasha doit-il vous appeler ?"}
            </label>
            <input
              id="wizard-how-to-call"
              className="onboarding-wizard-input"
              value={howToCall}
              onChange={(e) => setHowToCall(e.target.value)}
              placeholder={locale === "en" ? "Your name" : "Votre prénom"}
            />
          </>
        )}
        {step === 5 && (
          <p>
            {locale === "en"
              ? "Setup complete. Adjust providers, channels, and memory in Settings."
              : "Configuration terminée. Ajustez les providers, canaux et la mémoire dans Réglages."}
          </p>
        )}
        <div className="onboarding-actions onboarding-wizard-actions">
          {step > 0 ? (
            <button type="button" className="btn-secondary" onClick={() => setStep((s) => s - 1)}>
              {locale === "en" ? "Back" : "Retour"}
            </button>
          ) : null}
          {step < steps.length - 1 ? (
            <button
              type="button"
              className="btn-primary"
              disabled={step === 1 && !daemonOk}
              onClick={() => setStep((s) => s + 1)}
            >
              {locale === "en" ? "Next" : "Suivant"}
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
              {locale === "en" ? "Finish" : "Terminer"}
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
