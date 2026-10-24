import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Model = { id: string; provider: string; context_length: number; eligible: boolean; blocked_reason?: string };
type Source = { checked_at?: number; error?: string; models: Model[] };
type Status = { enabled: boolean; openrouter_ready: boolean; openrouter_fresh: boolean; catalog: { openrouter: Source; huggingface: Source } };

export function FreeModelsPanel({ port }: { port: number }) {
  const [status, setStatus] = useState<Status | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const version = useRef(0);
  const updating = useRef(false);
  const load = useCallback(async () => {
    if (updating.current) return;
    const current = ++version.current;
    try {
      const next = await invoke<Status>("free_models_control", { port, action: "status" });
      if (current === version.current) { setStatus(next); setError(null); }
    }
    catch (cause) { if (current === version.current) setError(String(cause)); }
  }, [port]);
  useEffect(() => { void load(); const timer = setInterval(() => { void load(); }, 30000); return () => clearInterval(timer); }, [load]);
  async function update(action: "set" | "refresh", enabled?: boolean) {
    if (updating.current) return;
    const confirmPaidRoutes = action === "set" && enabled === false;
    if (confirmPaidRoutes && !window.confirm("Désactiver le mode gratuit ? Les routes habituelles pourront utiliser des modèles payants. Aucun appel payant n’est lancé par cette action.")) return;
    updating.current = true; version.current += 1;
    setBusy(true); setError(null);
    try { setStatus(await invoke<Status>("free_models_control", { port, action, enabled, confirmPaidRoutes })); }
    catch (cause) { setError(String(cause)); }
    finally { updating.current = false; setBusy(false); }
  }
  return <section className="free-models-settings" aria-labelledby="free-models-title" aria-busy={busy}>
    <h3 id="free-models-title">Modèles gratuits</h3>
    <p>Privilégier les modèles gratuits compatibles avec la tâche. Aucun secours payant en mode gratuit ; les routes locales configurées restent disponibles.</p>
    {error && <p role="alert">{error}</p>}
    {!status ? <button type="button" className="btn-secondary" disabled={busy} onClick={() => void load()}>Charger les paramètres</button> : <>
      <label><input type="checkbox" checked={status.enabled} disabled={busy} onChange={event => void update("set", event.target.checked)} /> Activer le mode gratuit strict</label>
      <p>{status.enabled ? "Secours payant bloqué. Une désactivation explicite est nécessaire pour revenir aux routes habituelles." : "Mode désactivé : les routes habituelles s’appliquent."}</p>
      {!status.openrouter_ready && <p>Configurez votre clé OpenRouter dans le coffre ou OPENROUTER_API_KEY, puis redémarrez le daemon pour utiliser les modèles distants gratuits.</p>}
      <button type="button" className="btn-secondary" disabled={busy} onClick={() => void update("refresh")}>{busy ? "Actualisation…" : "Actualiser le catalogue"}</button>
      <p>Actualisation automatique au démarrage et toutes les heures lorsque le mode est actif. Les offres périmées ne sont pas sélectionnées.</p>
      {(["openrouter", "huggingface"] as const).map(provider => {
        const source = status.catalog[provider];
        return <details key={provider}><summary>{provider === "openrouter" ? "OpenRouter" : "Hugging Face"} — {source.models.length} offre(s) gratuite(s) détectée(s)</summary>
          <p>{source.checked_at ? `Dernière mise à jour : ${new Date(source.checked_at * 1000).toLocaleString("fr-FR")}` : "Catalogue non actualisé"}</p>
          {source.error && <p role="status">Actualisation échouée : {source.error}</p>}
          {provider === "openrouter" && !status.openrouter_fresh && <p>Catalogue périmé ou indisponible : sélection distante bloquée.</p>}
          {provider === "huggingface" && <p>Les crédits gratuits ne sont pas des modèles gratuits. Les promotions sont affichées, mais leur utilisation automatique reste bloquée faute de plafond tarifaire nul garanti par l’API.</p>}
          <ul>{source.models.map((model, index) => <li key={`${model.id}-${model.provider}-${index}`}>
            {model.id} — {model.context_length.toLocaleString("fr-FR")} tokens{model.blocked_reason ? ` — ${model.blocked_reason}` : ""}
          </li>)}</ul>
        </details>;
      })}
    </>}
  </section>;
}
