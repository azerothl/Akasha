import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Props = {
  locale: "fr" | "en";
  disabled?: boolean;
};

type EndpointResult = { ok: boolean; status: number; text: string };

export function OpenClawMigrationPanel({ locale, disabled }: Props) {
  const [sourceDir, setSourceDir] = useState("");
  const [busy, setBusy] = useState<null | "preview" | "apply">(null);
  const [result, setResult] = useState<string>("");

  const run = async (path: "/api/migrate/openclaw/preview" | "/api/migrate/openclaw/apply") => {
    const trimmed = sourceDir.trim();
    if (!trimmed) {
      setResult(locale === "en" ? "Please provide a source directory." : "Veuillez renseigner un dossier source.");
      return;
    }
    setBusy(path.endsWith("/preview") ? "preview" : "apply");
    setResult("");
    try {
      const res = await invoke<EndpointResult>("daemon_request", {
        method: "POST",
        path,
        body: JSON.stringify({ source_dir: trimmed }),
        port: 3876,
      });
      setResult(res.text || `${res.status}`);
    } catch (e) {
      setResult(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="settings-card">
      <h4>{locale === "en" ? "OpenClaw migration" : "Migration OpenClaw"}</h4>
      <p className="settings-doc muted">
        {locale === "en"
          ? "Import skills/tools policy from an OpenClaw directory."
          : "Importer les skills/la policy outils depuis un dossier OpenClaw."}
      </p>
      <label className="field">
        <span>{locale === "en" ? "Source directory" : "Dossier source"}</span>
        <input
          type="text"
          className="settings-input"
          value={sourceDir}
          onChange={(e) => setSourceDir(e.target.value)}
          placeholder={locale === "en" ? "C:\\path\\to\\openclaw" : "C:\\chemin\\vers\\openclaw"}
          disabled={disabled || !!busy}
        />
      </label>
      <div className="settings-row-actions">
        <button
          type="button"
          className="btn-secondary"
          disabled={disabled || !!busy}
          onClick={() => void run("/api/migrate/openclaw/preview")}
        >
          {busy === "preview" ? "…" : locale === "en" ? "Preview" : "Prévisualiser"}
        </button>
        <button
          type="button"
          className="btn-secondary"
          disabled={disabled || !!busy}
          onClick={() => void run("/api/migrate/openclaw/apply")}
        >
          {busy === "apply" ? "…" : locale === "en" ? "Apply" : "Appliquer"}
        </button>
      </div>
      {result ? <pre className="onboarding-doctor-output">{result}</pre> : null}
    </section>
  );
}
