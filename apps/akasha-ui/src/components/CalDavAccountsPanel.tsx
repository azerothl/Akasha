import { useCallback, useEffect, useMemo, useState } from "react";
import { InfoTip } from "./Tooltip";

type CalAccount = {
  id: string;
  label: string;
  url: string;
  username: string;
  provider_id?: string | null;
  enabled: boolean;
  last_sync_at?: string | null;
  last_sync_error?: string | null;
};

type ProviderPreset = {
  id: string;
  name_en: string;
  name_fr: string;
  url: string;
  url_editable: boolean;
  url_placeholder_en: string;
  url_placeholder_fr: string;
  username_hint_en: string;
  username_hint_fr: string;
  password_hint_en: string;
  password_hint_fr: string;
  docs_url?: string | null;
};

type SyncStatus = {
  connected?: boolean;
  last_sync_at?: string | null;
  last_error?: string | null;
  sidecar_enabled?: boolean;
};

type Props = {
  locale: "fr" | "en";
  fetchEndpoint: (path: string, init?: RequestInit) => Promise<{ ok: boolean; status: number; text: string }>;
};

const PROVIDER_ICONS: Record<string, string> = {
  google_calendar: "G",
  outlook: "O",
  apple_icloud: "",
  fastmail: "F",
  nextcloud: "N",
  custom: "+",
};

function providerName(p: ProviderPreset, locale: "fr" | "en") {
  return locale === "en" ? p.name_en : p.name_fr;
}

function pickLocale(p: ProviderPreset, locale: "fr" | "en", enKey: keyof ProviderPreset, frKey: keyof ProviderPreset) {
  return (locale === "en" ? p[enKey] : p[frKey]) as string;
}

export function CalDavAccountsPanel({ locale, fetchEndpoint }: Props) {
  const [accounts, setAccounts] = useState<CalAccount[]>([]);
  const [providers, setProviders] = useState<ProviderPreset[]>([]);
  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [selectedProviderId, setSelectedProviderId] = useState("google_calendar");
  const [label, setLabel] = useState("");
  const [url, setUrl] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [icsText, setIcsText] = useState("");
  const [message, setMessage] = useState<string | null>(null);

  const selectedProvider = useMemo(
    () => providers.find((p) => p.id === selectedProviderId) ?? providers[0],
    [providers, selectedProviderId],
  );

  const applyProvider = useCallback((p: ProviderPreset) => {
    setSelectedProviderId(p.id);
    setUrl(p.url);
  }, []);

  const load = useCallback(async () => {
    const [acc, sync, prov] = await Promise.all([
      fetchEndpoint("/api/calendar/accounts"),
      fetchEndpoint("/api/calendar/sync-status"),
      fetchEndpoint("/api/calendar/providers"),
    ]);
    if (acc.ok) {
      try {
        const j = JSON.parse(acc.text) as { accounts?: CalAccount[] };
        setAccounts(j.accounts ?? []);
      } catch {
        setAccounts([]);
      }
    }
    if (sync.ok) {
      try {
        setSyncStatus(JSON.parse(sync.text) as SyncStatus);
      } catch {
        setSyncStatus(null);
      }
    }
    if (prov.ok) {
      try {
        const j = JSON.parse(prov.text) as { providers?: ProviderPreset[] };
        setProviders(j.providers ?? []);
      } catch {
        setProviders([]);
      }
    }
  }, [fetchEndpoint]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (selectedProvider) {
      setUrl(selectedProvider.url);
    }
  }, [selectedProvider]);

  const providerLabelForAccount = (account: CalAccount) => {
    const pid = account.provider_id;
    if (!pid) return null;
    const p = providers.find((x) => x.id === pid);
    return p ? providerName(p, locale) : pid;
  };

  const addAccount = async () => {
    const effectiveUrl = url.trim();
    const effectiveUser = username.trim();
    if (!effectiveUrl || !effectiveUser) {
      setMessage(locale === "en" ? "URL and username are required." : "URL et utilisateur requis.");
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const res = await fetchEndpoint("/api/calendar/accounts", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          label: label.trim() || undefined,
          url: effectiveUrl,
          username: effectiveUser,
          provider_id: selectedProviderId,
        }),
      });
      if (!res.ok) {
        setMessage(res.text);
        return;
      }
      let accountId = "";
      try {
        const created = JSON.parse(res.text) as { id?: string };
        accountId = created.id ?? "";
      } catch {
        /* ignore */
      }
      if (password.trim() && accountId) {
        const vaultRes = await fetchEndpoint("/api/vault", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ key: `caldav_${accountId}_password`, value: password.trim() }),
        });
        if (!vaultRes.ok) {
          setMessage(
            locale === "en"
              ? `Account saved but vault password failed: ${vaultRes.text}`
              : `Compte enregistré mais mot de passe vault échoué : ${vaultRes.text}`,
          );
          await load();
          return;
        }
      }
      setMessage(
        password.trim()
          ? locale === "en"
            ? "Account and password saved. Start the caldav-channel sidecar to sync."
            : "Compte et mot de passe enregistrés. Lancez le sidecar caldav-channel pour synchroniser."
          : locale === "en"
            ? "Account saved. Add password in vault or below on next edit."
            : "Compte enregistré. Ajoutez le mot de passe dans le vault si besoin.",
      );
      setLabel("");
      setUsername("");
      setPassword("");
      if (selectedProvider) setUrl(selectedProvider.url);
      await load();
    } finally {
      setBusy(false);
    }
  };

  const importIcs = async () => {
    if (!icsText.trim()) return;
    setBusy(true);
    setMessage(null);
    try {
      const res = await fetchEndpoint("/api/calendar/ics", {
        method: "POST",
        headers: { "Content-Type": "text/calendar; charset=utf-8" },
        body: icsText,
      });
      setMessage(res.ok ? (locale === "en" ? "ICS imported." : "ICS importé.") : res.text);
      if (res.ok) setIcsText("");
    } finally {
      setBusy(false);
    }
  };

  const exportIcs = async () => {
    setBusy(true);
    try {
      const now = new Date();
      const from = new Date(now.getTime() - 30 * 86400000).toISOString();
      const to = new Date(now.getTime() + 90 * 86400000).toISOString();
      const res = await fetchEndpoint(`/api/calendar/ics?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`);
      if (res.ok) {
        const blob = new Blob([res.text], { type: "text/calendar" });
        const a = document.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = "akasha-calendar.ics";
        a.click();
        URL.revokeObjectURL(a.href);
      }
    } finally {
      setBusy(false);
    }
  };

  const txt = {
    title: locale === "en" ? "External calendar (CalDAV / ICS)" : "Calendrier externe (CalDAV / ICS)",
    addTitle: locale === "en" ? "Add a calendar" : "Ajouter un calendrier",
    pickProvider: locale === "en" ? "Choose your provider" : "Choisissez votre service",
    label: locale === "en" ? "Display name (optional)" : "Nom affiché (optionnel)",
    url: "CalDAV URL",
    username: locale === "en" ? "Email / username" : "E-mail / utilisateur",
    password: locale === "en" ? "App password" : "Mot de passe d'application",
    addBtn: locale === "en" ? "Connect calendar" : "Connecter le calendrier",
    icsSection: locale === "en" ? "Import / export ICS file" : "Import / export fichier ICS",
    customHint:
      locale === "en"
        ? "Custom server: enter the CalDAV URL and credentials from your provider."
        : "Serveur personnalisé : saisissez l'URL CalDAV et les identifiants de votre fournisseur.",
    sidecarHint:
      locale === "en"
        ? "After saving, run the caldav-channel sidecar with CALDAV_ACCOUNT_ID set to the account id."
        : "Après enregistrement, lancez le sidecar caldav-channel avec CALDAV_ACCOUNT_ID = id du compte.",
    docs: locale === "en" ? "Provider help" : "Aide du fournisseur",
    noAccounts: locale === "en" ? "No calendars connected yet." : "Aucun calendrier connecté.",
  };

  return (
    <section className="caldav-panel" aria-label={locale === "en" ? "CalDAV & ICS" : "CalDAV et ICS"}>
      <h3>{txt.title}</h3>
      {syncStatus && (
        <p className="caldav-sync-status">
          {locale === "en" ? "Sync" : "Sync"} :{" "}
          {syncStatus.connected ? "OK" : locale === "en" ? "idle" : "inactif"}
          {syncStatus.last_error ? ` — ${syncStatus.last_error}` : ""}
        </p>
      )}

      {accounts.length === 0 ? (
        <p className="caldav-hint">{txt.noAccounts}</p>
      ) : (
        <ul className="caldav-accounts-list">
          {accounts.map((a) => {
            const badge = providerLabelForAccount(a);
            return (
              <li key={a.id}>
                {badge && <span className="caldav-provider-badge">{badge}</span>}
                <strong>{a.label}</strong>
                <span className="caldav-account-meta">
                  {a.username}
                  {a.last_sync_error && <span className="caldav-error"> — {a.last_sync_error}</span>}
                </span>
                <button
                  type="button"
                  className="caldav-delete-btn"
                  disabled={busy}
                  onClick={() => void (async () => {
                    setBusy(true);
                    const res = await fetchEndpoint(`/api/calendar/accounts/${encodeURIComponent(a.id)}`, { method: "DELETE" });
                    setMessage(res.ok ? (locale === "en" ? "Account removed." : "Compte supprimé.") : res.text);
                    if (res.ok) await load();
                    setBusy(false);
                  })()}
                >
                  {locale === "en" ? "Remove" : "Supprimer"}
                </button>
              </li>
            );
          })}
        </ul>
      )}

      <div className="caldav-add-section">
        <h4>{txt.addTitle}</h4>
        <p className="caldav-hint">{txt.pickProvider}</p>
        <div className="caldav-provider-grid" role="listbox" aria-label={txt.pickProvider}>
          {providers.map((p) => (
            <button
              key={p.id}
              type="button"
              role="option"
              aria-selected={selectedProviderId === p.id}
              className={`caldav-provider-card${selectedProviderId === p.id ? " caldav-provider-card--active" : ""}`}
              onClick={() => applyProvider(p)}
            >
              <span className="caldav-provider-icon" aria-hidden="true">
                {PROVIDER_ICONS[p.id] ?? "·"}
              </span>
              <span className="caldav-provider-name">{providerName(p, locale)}</span>
            </button>
          ))}
        </div>

        {selectedProvider && (
          <div className="caldav-form">
            {selectedProvider.id === "custom" && <p className="caldav-hint">{txt.customHint}</p>}
            {selectedProvider.docs_url && (
              <p className="caldav-docs-link">
                <a href={selectedProvider.docs_url} target="_blank" rel="noopener noreferrer">
                  {txt.docs} — {providerName(selectedProvider, locale)}
                </a>
              </p>
            )}
            <label className="caldav-field">
              <span>{txt.label}</span>
              <input
                value={label}
                onChange={(e) => setLabel(e.target.value)}
                placeholder={selectedProvider ? providerName(selectedProvider, locale) : ""}
              />
            </label>
            <label className="caldav-field">
              <span>
                {txt.url}
                {selectedProvider && (
                  <InfoTip
                    label={txt.url}
                    content={pickLocale(selectedProvider, locale, "url_placeholder_en", "url_placeholder_fr") || txt.url}
                  />
                )}
              </span>
              <input
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                readOnly={selectedProvider ? !selectedProvider.url_editable && !!selectedProvider.url : false}
                placeholder={selectedProvider ? pickLocale(selectedProvider, locale, "url_placeholder_en", "url_placeholder_fr") : ""}
              />
            </label>
            <label className="caldav-field">
              <span>
                {txt.username}
                {selectedProvider && (
                  <InfoTip
                    label={txt.username}
                    content={pickLocale(selectedProvider, locale, "username_hint_en", "username_hint_fr")}
                  />
                )}
              </span>
              <input
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder={selectedProvider ? pickLocale(selectedProvider, locale, "username_hint_en", "username_hint_fr") : ""}
                autoComplete="username"
              />
            </label>
            <label className="caldav-field">
              <span>
                {txt.password}
                {selectedProvider && (
                  <InfoTip
                    label={txt.password}
                    content={pickLocale(selectedProvider, locale, "password_hint_en", "password_hint_fr")}
                  />
                )}
              </span>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={locale === "en" ? "Stored encrypted in vault" : "Stocké chiffré dans le vault"}
                autoComplete="new-password"
              />
            </label>
            <button type="button" className="caldav-connect-btn" disabled={busy} onClick={() => void addAccount()}>
              {txt.addBtn}
            </button>
          </div>
        )}
        <p className="caldav-hint">{txt.sidecarHint}</p>
      </div>

      <details className="caldav-ics-details">
        <summary>{txt.icsSection}</summary>
        <div className="caldav-ics">
          <textarea
            rows={4}
            placeholder=".ics content"
            value={icsText}
            onChange={(e) => setIcsText(e.target.value)}
            aria-label="ICS import"
          />
          <div className="caldav-ics-actions">
            <button type="button" disabled={busy} onClick={() => void importIcs()}>
              {locale === "en" ? "Import ICS" : "Importer ICS"}
            </button>
            <button type="button" disabled={busy} onClick={() => void exportIcs()}>
              {locale === "en" ? "Export ICS" : "Exporter ICS"}
            </button>
          </div>
        </div>
      </details>

      {message && <p className="caldav-message">{message}</p>}
    </section>
  );
}
