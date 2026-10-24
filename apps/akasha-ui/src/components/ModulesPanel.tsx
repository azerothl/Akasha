import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./ModulesPanel.css";

type AppModule = {
  id: string;
  name: string;
  description: string;
  version: string;
  kind: string;
  source_url: string;
  url: string;
  install_command: string;
  launch_command: string;
  requirements: string[];
  node_available: boolean;
  node_version?: string | null;
  node_supported: boolean;
  npm_available: boolean;
  installed: boolean;
  installed_version?: string | null;
  running: boolean;
};

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  locale: "fr" | "en";
};

const CODE_STUDIO_FALLBACK: AppModule = {
  id: "code-studio",
  name: "Akasha Code Studio",
  description: "Espace de travail de développement avec agents, éditeur, aperçu, Git et Kanban.",
  version: "0.10.0",
  kind: "web-app",
  source_url: "https://github.com/azerothl/akasha-code-studio",
  url: "http://127.0.0.1:5178/",
  install_command: "npm install --global akasha-code-studio@0.10.0",
  launch_command: "akasha-code-studio",
  requirements: ["Node.js 22.12+, 24.x ou 26+", "npm"],
  node_available: false,
  node_supported: false,
  npm_available: false,
  installed: false,
  running: false,
};

const isTauri = () => Boolean((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
const delay = (ms: number) => new Promise(resolve => window.setTimeout(resolve, ms));

type ModuleIconName = "open" | "check" | "download" | "external" | "puzzle" | "refresh" | "launch";
function ModuleIcon({ name, className }: { name: ModuleIconName; className?: string }) {
  const common = { className, width: 16, height: 16, viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: 1.8, strokeLinecap: "round" as const, strokeLinejoin: "round" as const, "aria-hidden": true as const };
  return <svg {...common}>
    {name === "open" && <><path d="M7 17 17 7" /><path d="M8 7h9v9" /></>}
    {name === "check" && <><circle cx="12" cy="12" r="9" /><path d="m8 12 2.5 2.5L16 9" /></>}
    {name === "download" && <><path d="M12 3v12" /><path d="m7 10 5 5 5-5" /><path d="M5 21h14" /></>}
    {name === "external" && <><path d="M14 4h6v6" /><path d="m20 4-9 9" /><path d="M18 13v6a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h6" /></>}
    {name === "puzzle" && <><path d="M8 3v3M16 3v3M8 18v3M16 18v3M3 8h3M3 16h3M18 8h3M18 16h3" /><rect x="6" y="6" width="12" height="12" rx="2" /><path d="M10 10h4v4h-4z" /></>}
    {name === "refresh" && <><path d="M20 7v5h-5" /><path d="M4 17v-5h5" /><path d="M5.6 9A7 7 0 0 1 18 6l2 2M4 16l2 2a7 7 0 0 0 12.4-3" /></>}
    {name === "launch" && <><path d="M5 4v16l14-8z" /></>}
  </svg>;
}

export function ModulesPanel({ fetchEndpoint, locale }: Props) {
  const en = locale === "en";
  const [moduleInfo, setModuleInfo] = useState<AppModule>(CODE_STUDIO_FALLBACK);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<"install" | "launch" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await fetchEndpoint("/api/modules");
      if (!response.ok) throw new Error(response.text || `HTTP ${response.status}`);
      const data = JSON.parse(response.text) as { modules?: AppModule[] };
      const codeStudio = data.modules?.find(module => module.id === "code-studio");
      if (codeStudio) setModuleInfo(codeStudio);
    } catch {
      setError(en
        ? "Could not check live module availability. The catalog entry is still shown below."
        : "Impossible de vérifier la disponibilité en direct. Le module reste affiché dans le catalogue.");
    }

    setLoading(false);
  }, [en, fetchEndpoint]);

  useEffect(() => { void refresh(); }, [refresh]);

  const openCodeStudio = async () => {
    if (!isTauri()) {
      window.open(CODE_STUDIO_FALLBACK.url, "_blank", "noopener,noreferrer");
      return;
    }
    await invoke("open_url", { url: CODE_STUDIO_FALLBACK.url });
  };

  const install = async () => {
    setBusy("install");
    setError(null);
    setNotice(null);
    try {
      if (!isTauri()) {
        await navigator.clipboard.writeText(moduleInfo.install_command);
        setNotice(en
          ? "Install command copied. Run it in a terminal, then refresh this page."
          : "Commande d’installation copiée. Exécutez-la dans un terminal puis actualisez cette page.");
      } else {
        await invoke("install_code_studio_module");
        setNotice(en ? "Akasha Code Studio is installed." : "Akasha Code Studio est installé.");
        await refresh();
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const launch = async () => {
    setBusy("launch");
    setError(null);
    setNotice(null);
    try {
      if (!isTauri()) {
        await navigator.clipboard.writeText(moduleInfo.launch_command);
        setNotice(en
          ? "Launch command copied. Run it in a terminal, then choose Open."
          : "Commande de lancement copiée. Exécutez-la dans un terminal puis choisissez Ouvrir.");
        return;
      }
      const startResponse = await fetchEndpoint("/api/modules/code-studio/launch", { method: "POST" });
      if (!startResponse.ok) {
        let message = startResponse.text || `HTTP ${startResponse.status}`;
        try { message = JSON.parse(startResponse.text).error ?? message; } catch { /* keep response text */ }
        throw new Error(message);
      }
      let running = false;
      try { running = (JSON.parse(startResponse.text) as { result?: { running?: boolean } }).result?.running === true; } catch { /* use the live status endpoint below */ }
      if (!running) {
        for (let attempt = 0; attempt < 20; attempt += 1) {
          await delay(500);
          const response = await fetchEndpoint("/api/modules");
          if (!response.ok) continue;
          const data = JSON.parse(response.text) as { modules?: AppModule[] };
          running = data.modules?.some(module => module.id === "code-studio" && module.running) ?? false;
          if (running) break;
        }
      }
      if (!running) throw new Error(en
        ? "The module process started but did not respond on 127.0.0.1:5178. Check that port 5178 is free and Node.js is supported."
        : "Le processus a démarré mais ne répond pas sur 127.0.0.1:5178. Vérifiez que le port 5178 est libre et que Node.js est compatible.");
      setModuleInfo(previous => ({ ...previous, running: true }));
      await openCodeStudio();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(null);
    }
  };

  const installed = moduleInfo.installed;
  const needsUpdate = installed && moduleInfo.installed_version !== moduleInfo.version;
  const running = moduleInfo.running;

  return (
    <div className="modules-panel">
      <div className="modules-heading-row">
        <div>
          <h2 id="modules-heading" className="panel-title">{en ? "App modules" : "Modules applicatifs"}</h2>
          <p className="panel-description">
            {en
              ? "Discover Akasha extensions, see what is ready, and open them from one place."
              : "Découvrez les extensions d’Akasha, voyez lesquelles sont prêtes et ouvrez-les depuis un seul endroit."}
          </p>
        </div>
        <button type="button" className="btn-secondary modules-refresh" onClick={() => void refresh()} disabled={loading || busy !== null}>
          <ModuleIcon name="refresh" /> {en ? "Refresh" : "Actualiser"}
        </button>
      </div>

      {error && <p className="modules-feedback modules-feedback-error" role="alert">{error}</p>}
      {notice && <p className="modules-feedback" role="status">{notice}</p>}

      <article className="module-card">
        <div className="module-card-icon" aria-hidden="true"><ModuleIcon name="puzzle" /></div>
        <div className="module-card-main">
          <div className="module-card-title-row">
            <div>
              <h3>{moduleInfo.name}</h3>
              <p className="module-card-description">{moduleInfo.description}</p>
            </div>
            <span className={`module-status ${running ? "module-status-running" : ""}`} role="status">
              <span aria-hidden="true" />
              {loading
                ? (en ? "Checking" : "Vérification")
                : running
                  ? (en ? "Running" : "En cours")
                  : installed
                    ? needsUpdate
                      ? (en ? "Update available" : "Mise à jour disponible")
                      : (en ? "Installed" : "Installé")
                    : (en ? "Available to install" : "Disponible à l’installation")}
            </span>
          </div>

          <dl className="module-details">
            <div><dt>{en ? "Version" : "Version"}</dt><dd>{moduleInfo.version}</dd></div>
              <div><dt>{en ? "Requires" : "Prérequis"}</dt><dd>{moduleInfo.requirements.join(" · ")}</dd></div>
            {moduleInfo.node_version && <div><dt>Node.js</dt><dd>{moduleInfo.node_version}{moduleInfo.node_supported ? "" : (en ? " · unsupported" : " · non pris en charge")}</dd></div>}
            {moduleInfo.installed_version && <div><dt>{en ? "Installed" : "Installée"}</dt><dd>{moduleInfo.installed_version}</dd></div>}
          </dl>

          <div className="module-card-actions">
            {(!installed || needsUpdate) && (
              <button type="button" className="btn-primary" onClick={() => void install()} disabled={busy !== null || loading || (isTauri() && (!moduleInfo.node_supported || !moduleInfo.npm_available))}>
                {busy === "install" ? <ModuleIcon name="refresh" className="modules-spin" /> : <ModuleIcon name="download" />}
                {busy === "install"
                  ? (en ? "Installing…" : "Installation…")
                  : isTauri()
                    ? installed
                      ? (en ? "Update module" : "Mettre à jour")
                      : (en ? "Install module" : "Installer le module")
                    : (en ? "Copy install command" : "Copier la commande d’installation")}
              </button>
            )}
            {installed && !needsUpdate && (
              <button type="button" className="btn-primary" onClick={() => void (running ? openCodeStudio() : launch())} disabled={busy !== null || (isTauri() && !running && !moduleInfo.node_supported)}>
                {busy === "launch" ? <ModuleIcon name="refresh" className="modules-spin" /> : running ? <ModuleIcon name="open" /> : <ModuleIcon name="launch" />}
                {running ? (en ? "Open Code Studio" : "Ouvrir Code Studio") : (en ? "Launch Code Studio" : "Lancer Code Studio")}
              </button>
            )}
            {running && (!installed || needsUpdate) && (
              <button type="button" className="btn-secondary" onClick={() => void openCodeStudio()}>
                <ModuleIcon name="open" /> {en ? "Open" : "Ouvrir"}
              </button>
            )}
            {isTauri() ? <button type="button" className="module-source-link" onClick={() => void invoke("open_url", { url: moduleInfo.source_url })}>
              <ModuleIcon name="external" /> {en ? "Official source" : "Source officielle"}
            </button> : <a className="module-source-link" href={moduleInfo.source_url} target="_blank" rel="noreferrer">
              <ModuleIcon name="external" /> {en ? "Official source" : "Source officielle"}
            </a>}
          </div>

          {!moduleInfo.node_available && !running && (
            <p className="module-requirement-note">
              {en
                ? "Node.js and npm are needed to install this module. Install Node.js 22.12+ and restart Akasha so the daemon reloads PATH."
                : "Node.js et npm sont nécessaires. Installez Node.js 22.12+ puis redémarrez Akasha pour recharger le PATH du daemon."}
            </p>
          )}
          {moduleInfo.node_available && !moduleInfo.node_supported && !running && (
            <p className="module-requirement-note">
              {en
                ? `Node.js ${moduleInfo.node_version} is unsupported. Install Node.js 22.12+, 24.x or 26+ and restart Akasha.`
                : `Node.js ${moduleInfo.node_version} n’est pas pris en charge. Installez Node.js 22.12+, 24.x ou 26+, puis redémarrez Akasha.`}
            </p>
          )}
          {moduleInfo.node_available && !moduleInfo.npm_available && !installed && (
            <p className="module-requirement-note">
              {en
                ? "npm is not available to the daemon. Install Node.js with npm, then restart Akasha."
                : "npm n’est pas disponible pour le daemon. Installez Node.js avec npm, puis redémarrez Akasha."}
            </p>
          )}
          {installed && !moduleInfo.npm_available && (
            <p className="module-requirement-note">{en ? "npm is no longer available on PATH; relaunching may fail." : "npm n’est plus disponible dans le PATH ; le lancement peut échouer."}</p>
          )}
          {needsUpdate && <p className="module-requirement-note">{en ? `Installed version ${moduleInfo.installed_version}; update to ${moduleInfo.version} before launching from here.` : `Version installée ${moduleInfo.installed_version} ; mettez-la à jour vers ${moduleInfo.version} avant de lancer le module ici.`}</p>}
        </div>
      </article>

      {!installed && (
        <p className="modules-install-note">
          <ModuleIcon name="check" />
          {en
            ? "Installation uses the pinned Akasha Code Studio package from the official npm registry. Akasha does not install or execute third-party catalog entries automatically."
            : "L’installation utilise la version Akasha Code Studio épinglée sur le registre npm officiel. Akasha n’installe ni n’exécute automatiquement les modules tiers."}
        </p>
      )}
    </div>
  );
}
