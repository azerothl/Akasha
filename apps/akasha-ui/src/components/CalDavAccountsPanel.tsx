import { useCallback, useEffect, useMemo, useState } from "react";
import { InfoTip } from "./Tooltip";

type CalAccount = {
  id: string;
  label: string;
  url: string;
  username: string;
  provider_id?: string | null;
  auth_method?: string | null;
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
  oauth_available?: boolean;
  oauth_label_en?: string;
  oauth_label_fr?: string;
};

type OAuthConfig = {
  google_calendar?: { oauth_configured?: boolean };
  outlook?: { oauth_configured?: boolean };
  redirect_uri?: string;
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
  const [oauthConfig, setOauthConfig] = useState<OAuthConfig | null>(null);
  const [authMode, setAuthMode] = useState<"password" | "oauth">("password");
  const [oauthPolling, setOauthPolling] = useState(false);
  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [selectedProviderId, setSelectedProviderId] = useState("google_calendar");
  const [label, setLabel] = useState("");
  const [url, setUrl] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [icsText, setIcsText] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const copyAccountId = async (id: string) => {
    try {
      await navigator.clipboard.writeText(id);
      setCopiedId(id);
      window.setTimeout(() => setCopiedId((cur) => (cur === id ? null : cur)), 2000);
    } catch {
      setCopiedId(null);
    }
  };

  const selectedProvider = useMemo(
    () => providers.find((p) => p.id === selectedProviderId) ?? providers[0],
    [providers, selectedProviderId],
  );

  const applyProvider = useCallback((p: ProviderPreset) => {
    setSelectedProviderId(p.id);
    setUrl(p.url);
    if (!p.oauth_available) setAuthMode("password");
  }, []);

  const oauthConfiguredFor = useCallback(
    (providerId: string) => {
      if (!oauthConfig) return false;
      if (providerId === "google_calendar") return !!oauthConfig.google_calendar?.oauth_configured;
      if (providerId === "outlook") return !!oauthConfig.outlook?.oauth_configured;
      return false;
    },
    [oauthConfig],
  );

  const canUseOAuth = selectedProvider?.oauth_available && oauthConfiguredFor(selectedProviderId);

  useEffect(() => {
    if (!canUseOAuth && authMode === "oauth") setAuthMode("password");
  }, [canUseOAuth, authMode]);

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
        const j = JSON.parse(prov.text) as { providers?: ProviderPreset[]; oauth?: OAuthConfig };
        setProviders(j.providers ?? []);
        setOauthConfig(j.oauth ?? null);
      } catch {
        setProviders([]);
        setOauthConfig(null);
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

  const startOAuth = async () => {
    if (!selectedProvider) return;
    setBusy(true);
    setMessage(null);
    setOauthPolling(true);
    try {
      const res = await fetchEndpoint("/api/calendar/oauth/start", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          provider_id: selectedProviderId,
          label: label.trim() || undefined,
        }),
      });
      if (!res.ok) {
        setMessage(
          locale === "en"
            ? "Could not start sign-in. Ask your administrator to configure OAuth client credentials."
            : "Impossible de lancer la connexion. Demandez à l'administrateur de configurer les identifiants OAuth.",
        );
        setOauthPolling(false);
        return;
      }
      const j = JSON.parse(res.text) as { auth_url?: string; state?: string };
      if (!j.auth_url || !j.state) {
        setOauthPolling(false);
        return;
      }
      window.open(j.auth_url, "_blank", "noopener,noreferrer");
      const deadline = Date.now() + 120_000;
      while (Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, 2000));
        const st = await fetchEndpoint(`/api/calendar/oauth/status?state=${encodeURIComponent(j.state!)}`);
        if (!st.ok) continue;
        const status = JSON.parse(st.text) as { status?: string; account_id?: string; error?: string };
        if (status.status === "completed") {
          setMessage(
            locale === "en"
              ? "Calendar connected with OAuth. Events will sync when the sync module is running."
              : "Calendrier connecté via OAuth. Les événements se synchroniseront lorsque le module de sync est actif.",
          );
          setLabel("");
          await load();
          break;
        }
        if (status.status === "error") {
          setMessage(status.error ?? (locale === "en" ? "OAuth failed." : "Échec OAuth."));
          break;
        }
      }
    } finally {
      setBusy(false);
      setOauthPolling(false);
    }
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
              ? "Calendar saved, but the password could not be stored securely. Try again or contact your administrator."
              : "Calendrier enregistré, mais le mot de passe n'a pas pu être stocké de façon sécurisée. Réessayez ou contactez l'administrateur.",
          );
          await load();
          return;
        }
      }
      setMessage(
        password.trim()
          ? locale === "en"
            ? "Calendar connected. If automatic sync is enabled on this machine, events will appear shortly. Otherwise, see the steps below or import a .ics file."
            : "Calendrier connecté. Si la synchronisation automatique est activée sur cette machine, les événements apparaîtront bientôt. Sinon, suivez les étapes ci-dessous ou importez un fichier .ics."
          : locale === "en"
            ? "Calendar saved. Add an app password to enable synchronization."
            : "Calendrier enregistré. Ajoutez un mot de passe d'application pour activer la synchronisation.",
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
    title: locale === "en" ? "External calendars" : "Calendriers externes",
    intro:
      locale === "en"
        ? "Link Google Calendar, Outlook, iCloud or another service. Events show up in the Akasha calendar with an « external » badge. For Google and Microsoft you can sign in with OAuth when configured; otherwise use an app password."
        : "Reliez Google Calendar, Outlook, iCloud ou un autre service. Les rendez-vous apparaissent dans le calendrier Akasha avec le badge « externe ». Pour Google et Microsoft, vous pouvez vous connecter via OAuth si c'est configuré ; sinon utilisez un mot de passe d'application.",
    addTitle: locale === "en" ? "Add a calendar" : "Ajouter un calendrier",
    pickProvider: locale === "en" ? "1. Choose your service" : "1. Choisissez votre service",
    label: locale === "en" ? "Display name (optional)" : "Nom affiché (optionnel)",
    url: locale === "en" ? "Server address" : "Adresse du serveur",
    username: locale === "en" ? "Email address" : "Adresse e-mail",
    password: locale === "en" ? "App password" : "Mot de passe d'application",
    addBtn: locale === "en" ? "Connect calendar" : "Connecter le calendrier",
    icsSection: locale === "en" ? "Without automatic sync: import / export .ics file" : "Sans sync auto : importer / exporter un fichier .ics",
    icsHint:
      locale === "en"
        ? "Export a calendar from Google or Outlook as .ics, paste it here, then click Import. No background sync required."
        : "Exportez un calendrier depuis Google ou Outlook en fichier .ics, collez-le ici puis cliquez Importer. Aucune sync en arrière-plan nécessaire.",
    customHint:
      locale === "en"
        ? "Enter the CalDAV address and credentials provided by your host."
        : "Saisissez l'adresse CalDAV et les identifiants fournis par votre hébergeur.",
    syncTitle: locale === "en" ? "2. Automatic synchronization" : "2. Synchronisation automatique",
    syncIntro:
      locale === "en"
        ? "After connecting, events are fetched periodically by a small sync helper on this computer (set up once by whoever manages Akasha)."
        : "Après la connexion, les événements sont récupérés régulièrement par un petit programme de synchronisation sur cet ordinateur (à configurer une fois par la personne qui administre Akasha).",
    syncSteps:
      locale === "en"
        ? [
            "Save the calendar above with your app password.",
            "Ask your Akasha administrator to enable calendar sync (caldav-channel plugin), or follow the advanced guide below.",
            "Within a few minutes, events should appear in the calendar grid.",
          ]
        : [
            "Enregistrez le calendrier ci-dessus avec votre mot de passe d'application.",
            "Demandez à l'administrateur Akasha d'activer la synchronisation calendrier (plugin caldav-channel), ou suivez le guide avancé ci-dessous.",
            "Après quelques minutes, les événements devraient apparaître dans la grille du calendrier.",
          ],
    syncStatusLabel: locale === "en" ? "Sync status" : "État de la sync",
    syncOk: locale === "en" ? "Synchronization active" : "Synchronisation active",
    syncIdle: locale === "en" ? "Waiting — sync helper not running yet" : "En attente — le module de sync n'est pas encore lancé",
    advancedTitle: locale === "en" ? "Advanced setup (administrators)" : "Configuration avancée (administrateurs)",
    advancedHint:
      locale === "en"
        ? "Technical details for installing and running the caldav-channel sync helper."
        : "Détails techniques pour installer et lancer le module de synchronisation caldav-channel.",
    accountIdLabel: locale === "en" ? "Account identifier (copy for sync config)" : "Identifiant du compte (à copier pour la config sync)",
    copyId: locale === "en" ? "Copy" : "Copier",
    copied: locale === "en" ? "Copied" : "Copié",
    docs: locale === "en" ? "How to create an app password" : "Créer un mot de passe d'application",
    noAccounts: locale === "en" ? "No calendars connected yet." : "Aucun calendrier connecté pour le moment.",
    passwordPlaceholder: locale === "en" ? "Stored securely on this computer" : "Stocké de façon sécurisée sur cet ordinateur",
    authModeTitle: locale === "en" ? "2. Sign-in method" : "2. Mode de connexion",
    authOAuth: locale === "en" ? "Sign in (OAuth)" : "Connexion OAuth",
    authPassword: locale === "en" ? "App password" : "Mot de passe d'application",
    oauthBtn: locale === "en" ? "Open sign-in page" : "Ouvrir la page de connexion",
    oauthWaiting: locale === "en" ? "Waiting for sign-in in your browser…" : "En attente de la connexion dans votre navigateur…",
    oauthNotConfigured:
      locale === "en"
        ? "OAuth is not configured on this Akasha instance. Use an app password, or ask your administrator to add OAuth client credentials to the vault."
        : "OAuth n'est pas configuré sur cette instance. Utilisez un mot de passe d'application, ou demandez à l'administrateur d'ajouter les identifiants OAuth dans le coffre-fort.",
    oauthBadge: "OAuth",
  };

  return (
    <section className="caldav-panel" aria-label={locale === "en" ? "External calendars" : "Calendriers externes"}>
      <h3>{txt.title}</h3>
      <p className="caldav-intro">{txt.intro}</p>
      {syncStatus && (
        <p className={`caldav-sync-status${syncStatus.connected ? " caldav-sync-status--ok" : ""}`}>
          <strong>{txt.syncStatusLabel} :</strong>{" "}
          {syncStatus.connected ? txt.syncOk : txt.syncIdle}
          {syncStatus.last_error ? (
            <span className="caldav-error">
              {" "}
              — {locale === "en" ? "Last error" : "Dernière erreur"} : {syncStatus.last_error}
            </span>
          ) : null}
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
                {a.auth_method === "oauth" && <span className="caldav-oauth-badge">{txt.oauthBadge}</span>}
                <strong>{a.label}</strong>
                <span className="caldav-account-meta">
                  {a.username}
                  {a.last_sync_at && (
                    <span className="caldav-last-sync">
                      {" "}
                      · {locale === "en" ? "Last sync" : "Dernière sync"} :{" "}
                      {new Date(a.last_sync_at).toLocaleString(locale === "en" ? "en-GB" : "fr-FR")}
                    </span>
                  )}
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
            {selectedProvider.docs_url && authMode === "password" && (
              <p className="caldav-docs-link">
                <a href={selectedProvider.docs_url} target="_blank" rel="noopener noreferrer">
                  {txt.docs} — {providerName(selectedProvider, locale)}
                </a>
              </p>
            )}

            {selectedProvider.oauth_available && (
              <div className="caldav-auth-mode">
                <span className="caldav-auth-mode-label">{txt.authModeTitle}</span>
                <div className="caldav-auth-mode-tabs" role="tablist">
                  <button
                    type="button"
                    role="tab"
                    aria-selected={authMode === "oauth"}
                    className={authMode === "oauth" ? "active" : ""}
                    disabled={!canUseOAuth}
                    onClick={() => setAuthMode("oauth")}
                  >
                    {locale === "en" ? selectedProvider.oauth_label_en || txt.authOAuth : selectedProvider.oauth_label_fr || txt.authOAuth}
                  </button>
                  <button
                    type="button"
                    role="tab"
                    aria-selected={authMode === "password"}
                    className={authMode === "password" ? "active" : ""}
                    onClick={() => setAuthMode("password")}
                  >
                    {txt.authPassword}
                  </button>
                </div>
                {selectedProvider.oauth_available && !canUseOAuth && (
                  <p className="caldav-hint">{txt.oauthNotConfigured}</p>
                )}
              </div>
            )}

            <label className="caldav-field">
              <span>{txt.label}</span>
              <input
                value={label}
                onChange={(e) => setLabel(e.target.value)}
                placeholder={selectedProvider ? providerName(selectedProvider, locale) : ""}
              />
            </label>

            {authMode === "oauth" && canUseOAuth ? (
              <div className="caldav-oauth-panel">
                <p className="caldav-hint">
                  {locale === "en"
                    ? "A browser window will open. Sign in with your usual Google or Microsoft account and approve calendar access."
                    : "Une fenêtre du navigateur va s'ouvrir. Connectez-vous avec votre compte Google ou Microsoft et autorisez l'accès au calendrier."}
                </p>
                <button
                  type="button"
                  className="caldav-connect-btn"
                  disabled={busy || oauthPolling}
                  onClick={() => void startOAuth()}
                >
                  {oauthPolling ? txt.oauthWaiting : txt.oauthBtn}
                </button>
              </div>
            ) : (
              <>
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
                placeholder={txt.passwordPlaceholder}
                autoComplete="new-password"
              />
            </label>
            <button type="button" className="caldav-connect-btn" disabled={busy} onClick={() => void addAccount()}>
              {txt.addBtn}
            </button>
              </>
            )}
          </div>
        )}

        <div className="caldav-sync-guide">
          <h4>{txt.syncTitle}</h4>
          <p className="caldav-hint">{txt.syncIntro}</p>
          <ol className="caldav-steps">
            {txt.syncSteps.map((step) => (
              <li key={step}>{step}</li>
            ))}
          </ol>
        </div>

        {accounts.length > 0 && (
          <details className="caldav-advanced">
            <summary>{txt.advancedTitle}</summary>
            <p className="caldav-hint">{txt.advancedHint}</p>
            <ul className="caldav-advanced-list">
              {accounts.map((a) => (
                <li key={a.id}>
                  <span className="caldav-advanced-label">
                    {a.label} — {txt.accountIdLabel}
                  </span>
                  <code className="caldav-account-id">{a.id}</code>
                  <button type="button" className="caldav-copy-btn" onClick={() => void copyAccountId(a.id)}>
                    {copiedId === a.id ? txt.copied : txt.copyId}
                  </button>
                </li>
              ))}
            </ul>
            <p className="caldav-hint caldav-advanced-cli muted">
              {locale === "en"
                ? "Admin: install the caldav-channel plugin, set CALDAV_ACCOUNT_ID to the id above, and run the sync helper. See Akasha_plugins/caldav-channel/README.md."
                : "Admin : installez le plugin caldav-channel, indiquez CALDAV_ACCOUNT_ID avec l'identifiant ci-dessus, puis lancez le module de sync. Voir Akasha_plugins/caldav-channel/README.md."}
            </p>
          </details>
        )}
      </div>

      <details className="caldav-ics-details">
        <summary>{txt.icsSection}</summary>
        <p className="caldav-hint">{txt.icsHint}</p>
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
