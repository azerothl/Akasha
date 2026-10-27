import { useEffect, useState } from "react";

export type RemoteConsent = { recipient: string; data_classes: string[] };
type Service = RemoteConsent & { name: string; endpoint: string; local: boolean };
type Grant = RemoteConsent & { scope: string; expires_at: string; revoked: boolean };
type RemoteRequest = RemoteConsent & { scope: string };
type Privacy = {
  services: Service[];
  search_services: Service[];
  grants: Grant[];
  requests?: RemoteRequest[];
  memory: { state: string; encrypted_at_rest: boolean };
};
export type PrivacyFetch = (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
function dataDescription(classes: string[], en: boolean): string {
  const names: Record<string, string> = en ? { message: "message", history: "conversation history", memory: "retrieved memories", documents: "documents", tool_results: "tool results", web_query: "search query", web_url: "requested page address", connector_payload: "content sent to the connector" } : { message: "message", history: "historique de conversation", memory: "souvenirs consultés", documents: "documents", tool_results: "résultats d’outils", web_query: "requête de recherche", web_url: "adresse de la page demandée", connector_payload: "contenu transmis au connecteur" };
  return classes.map((value) => names[value] ?? value).join(", ");
}

function usePrivacy(fetchEndpoint: PrivacyFetch, refresh = 0) {
  const [state, setState] = useState<Privacy | null>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    let disposed = false;
    fetchEndpoint("/api/privacy").then((response) => {
      if (!response.ok) throw new Error("privacy unavailable");
      const value = JSON.parse(response.text) as Privacy;
      if (!Array.isArray(value.services) || !value.memory) throw new Error("invalid privacy state");
      if (!disposed) { setState(value); setError(false); }
    }).catch(() => { if (!disposed) { setState(null); setError(true); } });
    return () => { disposed = true; };
  }, [fetchEndpoint, refresh]);
  return { state, error };
}

/** Never persisted: each outgoing task starts with unchecked remote permissions. */
export function RemoteConsentPicker({ fetchEndpoint, value, onChange, disabled, locale }: {
  fetchEndpoint: PrivacyFetch; value: RemoteConsent[]; onChange: (value: RemoteConsent[]) => void;
  disabled: boolean; locale: "fr" | "en";
}) {
  const { state, error } = usePrivacy(fetchEndpoint);
  const en = locale === "en";
  const services = state ? [...state.services, ...(state.search_services ?? [])].filter((service) => !service.local) : [];
  return <details className="remote-privacy">
    <summary>{value.length ? (en ? "Remote services allowed for this request" : "Services distants autorisés pour cette demande") : (en ? "Local models · remote services blocked" : "Modèles locaux · services distants bloqués")}</summary>
    {error ? <p role="alert">{en ? "Privacy status unavailable. Remote access stays blocked." : "État de confidentialité indisponible. Les envois distants restent bloqués."}</p> : null}
    {!state && !error ? <p role="status">{en ? "Checking permissions…" : "Vérification des autorisations…"}</p> : null}
    <p>{en ? "An API key is not permission. Choose each recipient for this task and its retries; this choice resets after sending." : "Une clé API ne vaut pas autorisation. Choisissez les destinataires pour cette tâche et ses tentatives. Ce choix est effacé après l’envoi."}</p>
    {services.map((service) => <label className="remote-privacy-choice" key={service.recipient}>
      <input type="checkbox" disabled={disabled} checked={value.some((item) => item.recipient === service.recipient)} onChange={(event) => {
        onChange(event.target.checked ? [...value, { recipient: service.recipient, data_classes: service.data_classes }] : value.filter((item) => item.recipient !== service.recipient));
      }} />
      <span><strong>{service.data_classes.includes("web_query") ? (en ? "Web search" : "Recherche Web") : (en ? "Remote model" : "Modèle distant")} · {service.name}</strong><br />{service.endpoint}<br />
        {service.data_classes.includes("web_query") ? (en ? "Search query only. Opening a result requires separate permission." : "Requête de recherche seulement. Ouvrir un résultat exige une autorisation distincte.") : (en ? "May receive your message, conversation history, retrieved memories, documents and tool results." : "Peut recevoir votre message, l’historique, les souvenirs consultés, les documents et les résultats d’outils.")}</span>
    </label>)}
    {state && services.length === 0 ? <p>{en ? "No remote service configured." : "Aucun service distant configuré."}</p> : null}
  </details>;
}

export function RemotePermissions({ fetchEndpoint, locale }: { fetchEndpoint: PrivacyFetch; locale: "fr" | "en" }) {
  const [refresh, setRefresh] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const { state, error: unavailable } = usePrivacy(fetchEndpoint, refresh);
  const en = locale === "en";
  const grants = state?.grants.filter((grant) => !grant.revoked && Date.parse(grant.expires_at) > Date.now()) ?? [];
  async function decide(grant: RemoteRequest, allow: boolean) {
    setBusy(true); setError("");
    try {
      const result = await fetchEndpoint(allow ? "/api/permissions/remote" : "/api/permissions/remote/revoke", { method: "POST", body: JSON.stringify(allow ? { confirmed: true, scope: grant.scope, consents: [{ recipient: grant.recipient, data_classes: grant.data_classes }] } : { scope: grant.scope, recipient: grant.recipient }) });
      if (!result.ok) throw new Error("revocation failed");
      setRefresh((value) => value + 1);
    } catch { setError(en ? "Could not save this permission. Try again." : "Modification de l’autorisation impossible. Réessayez."); }
    finally { setBusy(false); }
  }
  return <section className="remote-privacy" aria-label={en ? "Effective privacy" : "Confidentialité effective"}>
    <h3>{en ? "Privacy" : "Confidentialité"}</h3>
    {unavailable ? <p role="alert">{en ? "Privacy status unavailable." : "État de confidentialité indisponible."}</p> : null}
    {state ? <>
      <p>{state.memory.encrypted_at_rest ? (en ? "Long-term memory encrypted." : "Mémoire long terme chiffrée.") : state.memory.state === "locked" ? (en ? "Memory locked: key unavailable." : "Mémoire verrouillée : clé indisponible.") : (en ? "Long-term memory is not protected yet." : "La mémoire long terme doit encore être protégée.")}</p>
      <p>{en ? "During the beta, conversations, notes and imported documents are not encrypted by Akasha. Your Windows account protects these files." : "Pendant la bêta, Akasha ne chiffre pas les conversations, notes et documents importés. Votre compte Windows protège ces fichiers."}</p>
      <h4>{en ? "Remote permissions" : "Autorisations distantes"}</h4>
      {(state.requests ?? []).map((request) => <div className="remote-privacy-grant" key={`request:${request.scope}:${request.recipient}`}>
        <p>{en ? "Blocked recipient" : "Destinataire bloqué"} : {request.recipient}<br />
          {dataDescription(request.data_classes, en)}<br />
          <small>{en ? "After approval, resume the task from Tasks." : "Après autorisation, reprenez la tâche depuis Tâches."}</small></p>
        <button type="button" className="btn-secondary" disabled={busy} onClick={() => void decide(request, true)}>{en ? "Allow for this task" : "Autoriser pour cette tâche"}</button>
      </div>)}
      {grants.length === 0 ? <p>{en ? "No active permission." : "Aucune autorisation active."}</p> : grants.map((grant) => <div key={`${grant.scope}:${grant.recipient}`} className="remote-privacy-grant">
        <p>{grant.recipient}<br /><small>{grant.scope.startsWith("schedule:") ? (en ? "Recurring task" : "Récurrence") : (en ? "Task" : "Tâche")} · {grant.scope.split(":")[1]?.slice(-8)} · {new Date(grant.expires_at).toLocaleString(locale)}</small></p>
        <button type="button" className="btn-secondary" disabled={busy} onClick={() => void decide(grant, false)}>{en ? "Revoke" : "Révoquer"}</button>
      </div>)}
    </> : null}
    {error ? <p role="alert">{error}</p> : null}
  </section>;
}
