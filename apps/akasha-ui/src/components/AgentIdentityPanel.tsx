import { useCallback, useEffect, useState } from "react";

type Props = {
  locale: "fr" | "en";
  fetchEndpoint: (method: string, path: string, body?: string) => Promise<{ ok: boolean; text: string }>;
};

type Manifest = {
  name?: string;
  mission?: string;
  values?: string[];
  boundaries?: string[];
};

export function AgentIdentityPanel({ locale, fetchEndpoint }: Props) {
  const [manifest, setManifest] = useState<Manifest>({});
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const load = useCallback(async () => {
    const res = await fetchEndpoint("GET", "/api/agent-identity");
    if (!res.ok) return;
    try {
      setManifest(JSON.parse(res.text) as Manifest);
    } catch {
      /* ignore */
    }
  }, [fetchEndpoint]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    setBusy(true);
    setMsg(null);
    const res = await fetchEndpoint("POST", "/api/agent-identity", JSON.stringify(manifest));
    setBusy(false);
    setMsg(
      res.ok
        ? locale === "en"
          ? "Agent identity saved."
          : "Identité agent enregistrée."
        : res.text.slice(0, 200),
    );
  };

  return (
    <div className="agent-identity-panel">
      <h3 className="settings-subtitle">
        {locale === "en" ? "Agent identity manifest" : "Manifeste identité agent"}
      </h3>
      <p className="settings-doc muted">
        {locale === "en"
          ? "Long-term agent identity (constitution layer). Stored as agent_identity.yaml."
          : "Identité agent long terme (couche constitution). Stockée dans agent_identity.yaml."}
      </p>
      <label>
        {locale === "en" ? "Name" : "Nom"}
        <input
          className="settings-input"
          value={manifest.name ?? ""}
          onChange={(e) => setManifest((m) => ({ ...m, name: e.target.value }))}
        />
      </label>
      <label>
        {locale === "en" ? "Mission" : "Mission"}
        <textarea
          className="settings-input"
          rows={3}
          value={manifest.mission ?? ""}
          onChange={(e) => setManifest((m) => ({ ...m, mission: e.target.value }))}
        />
      </label>
      <button type="button" className="btn-primary" disabled={busy} onClick={() => void save()}>
        {busy ? "…" : locale === "en" ? "Save identity" : "Enregistrer"}
      </button>
      {msg ? <p className="muted">{msg}</p> : null}
    </div>
  );
}
