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
      ? ["Welcome", "Health check", "Provider hint", "Your name", "Ready"]
      : ["Bienvenue", "Diagnostic", "Conseil provider", "Votre prénom", "Prêt"];

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
                ? "Tip: configure your preferred LLM provider in Settings > System when setup is done."
                : "Conseil : configurez votre provider LLM préféré dans Réglages > Système une fois la configuration terminée."}
            </p>
            <p className="muted">
              {locale === "en"
                ? "Optional services (Ollama, external APIs, plugin connectors) can be installed later."
                : "Les services optionnels (Ollama, APIs externes, connecteurs plugins) peuvent être installés plus tard."}
            </p>
          </>
        )}
        {step === 3 && (
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
        {step === 4 && (
          <p>
            {locale === "en"
              ? "Setup complete. You can adjust providers and memory in Settings."
              : "Configuration terminée. Ajustez les providers et la mémoire dans Réglages."}
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
