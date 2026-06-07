import { useCallback, useEffect, useState } from "react";

type CatalogEntry = {
  id?: string;
  name?: string;
  description?: string;
  version?: string;
};

type Props = {
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
  requestEndpoint: (method: string, path: string, body?: string) => Promise<{ ok: boolean; status: number; text: string }>;
  installedIds: Set<string>;
  onInstalled: () => void;
  locale: "fr" | "en";
};

export function PluginCatalogPanel({
  fetchEndpoint,
  requestEndpoint,
  installedIds,
  onInstalled,
  locale,
}: Props) {
  const [catalog, setCatalog] = useState<CatalogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [installing, setInstalling] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await fetchEndpoint("/api/plugins/catalog");
      if (!res.ok) {
        setError(res.text || `HTTP ${res.status}`);
        return;
      }
      const j = JSON.parse(res.text) as { plugins?: CatalogEntry[] };
      setCatalog(Array.isArray(j.plugins) ? j.plugins : []);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [fetchEndpoint]);

  useEffect(() => {
    void load();
  }, [load]);

  const install = async (id: string) => {
    setInstalling(id);
    setError(null);
    try {
      const res = await requestEndpoint("POST", "/api/plugins/install", JSON.stringify({ id }));
      if (!res.ok) {
        setError(res.text || `HTTP ${res.status}`);
        return;
      }
      onInstalled();
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setInstalling(null);
    }
  };

  return (
    <div className="plugin-catalog-panel">
      <h4>{locale === "en" ? "Marketplace (catalog)" : "Marketplace (catalogue)"}</h4>
      {loading ? <p className="muted">{locale === "en" ? "Loading…" : "Chargement…"}</p> : null}
      {error ? <p className="steering-queue-error">{error}</p> : null}
      {!loading && catalog.length === 0 ? (
        <p className="muted">{locale === "en" ? "No catalog entries." : "Catalogue vide."}</p>
      ) : (
        <ul className="plugin-catalog-list">
          {catalog.map((p) => {
            const id = p.id ?? "";
            const installed = installedIds.has(id);
            return (
              <li key={id} className="plugin-catalog-item">
                <div>
                  <strong>{p.name ?? id}</strong>
                  {p.version ? <span className="plugin-catalog-version"> v{p.version}</span> : null}
                  {p.description ? <p className="muted plugin-catalog-desc">{p.description}</p> : null}
                </div>
                <button
                  type="button"
                  className="btn-secondary"
                  disabled={installed || installing === id || !id}
                  onClick={() => void install(id)}
                >
                  {installed
                    ? locale === "en"
                      ? "Installed"
                      : "Installé"
                    : installing === id
                      ? "…"
                      : locale === "en"
                        ? "Install"
                        : "Installer"}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
