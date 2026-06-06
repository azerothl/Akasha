import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type UserRagDocument = {
  id: string;
  name: string;
  mime_type: string;
  added_at: string;
  index_status?: string;
  indexed_at?: string | null;
  index_error?: string | null;
};

type FetchEndpoint = (
  path: string,
  init?: RequestInit
) => Promise<{ ok: boolean; status: number; text: string }>;

type Props = {
  t: (key: string) => string;
  locale: string;
  daemonPort: number | undefined;
  documents: UserRagDocument[];
  loading: boolean;
  error: string | null;
  onError: (msg: string | null) => void;
  onRefresh: () => void;
  fetchEndpoint: FetchEndpoint;
  readFileAsBase64: (file: File) => Promise<{ content_base64: string; mime_type: string }>;
};

export function UserRagPanel({
  t,
  locale,
  daemonPort,
  documents,
  loading,
  error,
  onError,
  onRefresh,
  fetchEndpoint,
  readFileAsBase64,
}: Props) {
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [searchBusy, setSearchBusy] = useState(false);
  const [searchText, setSearchText] = useState("");

  const handleUpload = useCallback(
    async (file: File) => {
      try {
        const { content_base64, mime_type } = await readFileAsBase64(file);
        await invoke("add_user_rag_document", {
          name: file.name,
          content_base64,
          mime_type,
          port: daemonPort,
        });
        onRefresh();
      } catch (err) {
        onError(String(err));
      }
    },
    [daemonPort, onError, onRefresh, readFileAsBase64]
  );

  const handleSearch = useCallback(async () => {
    const q = query.trim();
    if (!q || searchBusy) return;
    setSearchBusy(true);
    setSearchText("");
    try {
      let res = await fetchEndpoint(
        `/api/user-rag/retrieve?q=${encodeURIComponent(q)}&top_k=5`
      );
      if (!res.ok) {
        res = await fetchEndpoint(
          `/api/memory/search?q=${encodeURIComponent(q)}&top_k=5`
        );
      }
      setSearchText(res.text || `HTTP ${res.status}`);
    } catch (e) {
      setSearchText(String(e));
    } finally {
      setSearchBusy(false);
    }
  }, [fetchEndpoint, query, searchBusy]);

  const indexStatusLabel = (status?: string) => {
    if (!status) return "";
    if (status === "indexed") return locale === "en" ? "indexed" : "indexé";
    if (status === "pending") return locale === "en" ? "pending" : "en attente";
    if (status === "error") return locale === "en" ? "error" : "erreur";
    return status;
  };

  return (
    <>
      <h3 className="settings-subtitle">{t("settings.user_rag_title")}</h3>
      <p className="settings-doc muted">{t("settings.user_rag_desc")}</p>
      {error ? (
        <p className="settings-doc" role="alert">
          {error}
        </p>
      ) : null}
      <input
        ref={fileInputRef}
        type="file"
        accept=".txt,.md,.csv,.json,text/*"
        className="sr-only"
        aria-hidden
        onChange={async (e) => {
          const file = e.target.files?.[0];
          if (!file) return;
          await handleUpload(file);
          e.target.value = "";
        }}
      />
      <button
        type="button"
        className="refresh-btn"
        onClick={() => fileInputRef.current?.click()}
        disabled={loading}
      >
        {t("settings.add_document")}
      </button>
      <div className="sidebar-right-search-wrap">
        <input
          type="search"
          className="sidebar-right-search"
          placeholder={t("settings.user_rag_search_placeholder")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void handleSearch();
          }}
        />
        <button
          type="button"
          className="btn-secondary"
          disabled={searchBusy || !query.trim()}
          onClick={() => void handleSearch()}
        >
          {searchBusy ? "…" : t("settings.user_rag_test_search")}
        </button>
      </div>
      {searchText ? <pre className="onboarding-doctor-output">{searchText}</pre> : null}
      {loading && (
        <p className="panel-loading" aria-busy="true">
          {t("common.loading")}
        </p>
      )}
      {!loading && documents.length === 0 && (
        <p className="empty-state">{t("settings.no_documents")}</p>
      )}
      {!loading && documents.length > 0 && (
        <ul className="settings-doc-list" role="list">
          {documents.map((d) => (
            <li key={d.id} className="settings-doc-item">
              <span className="settings-doc-name">{d.name}</span>
              <span className="settings-doc-meta">
                {d.added_at.slice(0, 10)}
                {d.index_status ? ` · ${indexStatusLabel(d.index_status)}` : ""}
                {d.index_error ? ` · ${d.index_error}` : ""}
              </span>
              <button
                type="button"
                className="settings-doc-delete"
                aria-label={`${t("settings.delete")} ${d.name}`}
                onClick={async () => {
                  try {
                    await invoke("delete_user_rag_document", { id: d.id, port: daemonPort });
                    onRefresh();
                  } catch (err) {
                    onError(String(err));
                  }
                }}
              >
                {t("settings.delete")}
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
