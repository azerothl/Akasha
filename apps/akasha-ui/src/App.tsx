import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

const DAEMON_PORT = 3876;

type Tab = "chat" | "router" | "settings" | "docs" | "activity";

/** French label for Activity event types (delegation, progress, etc.). */
function eventTypeLabel(typ: string): string {
  const labels: Record<string, string> = {
    user_request_received: "Demande reçue",
    acknowledgment_sent: "Accusé de réception envoyé",
    task_created: "Tâche créée",
    task_decomposed: "Tâche décomposée (délégation à des sous-agents)",
    sub_agent_spawned: "Délégué à un agent spécialisé",
    progress_update: "Progression",
    task_completed: "Tâche terminée",
    task_failed: "Tâche en échec",
  };
  return labels[typ] ?? typ;
}

interface HealthState {
  ok: boolean;
  port?: number;
}

interface ModelMetricsEntry {
  total_requests: number;
  successful_requests: number;
  failed_requests: number;
  total_latency_ms: number;
  total_tokens: number;
  total_cost_usd: number;
  fallback_triggered: number;
  fallback_success: number;
  last_success?: string;
  last_failure?: string;
}

type RouterMetrics = Record<string, ModelMetricsEntry>;

function App() {
  const [tab, setTab] = useState<Tab>("chat");
  const [health, setHealth] = useState<HealthState | null>(null);
  const [message, setMessage] = useState("");
  const [messages, setMessages] = useState<
    Array<{ role: "user" | "assistant" | "system"; text: string; error?: boolean }>
  >([]);
  const [loading, setLoading] = useState(false);
  const [routerMetrics, setRouterMetrics] = useState<RouterMetrics | null>(null);
  const [routerLoading, setRouterLoading] = useState(false);
  const [routerError, setRouterError] = useState<string | null>(null);
  const [docContent, setDocContent] = useState<string | null>(null);
  const [docLoading, setDocLoading] = useState(false);
  const [docError, setDocError] = useState<string | null>(null);
  const [activityTasks, setActivityTasks] = useState<Array<{ id: string; status: string }>>([]);
  const [activitySelected, setActivitySelected] = useState(0);
  const [activityEvents, setActivityEvents] = useState<Array<{ event_type: string; payload?: unknown; at: string }>>([]);
  const [activityLoading, setActivityLoading] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const chatEndRef = useRef<HTMLDivElement>(null);
  const chatInputRef = useRef<HTMLInputElement>(null);

  const checkHealth = useCallback(async () => {
    try {
      const result = await invoke<{ ok: boolean; port?: number }>("check_health", {
        port: DAEMON_PORT,
      });
      setHealth({ ok: result.ok, port: result.port ?? DAEMON_PORT });
    } catch {
      setHealth({ ok: false, port: DAEMON_PORT });
    }
  }, []);

  useEffect(() => {
    checkHealth();
    const id = setInterval(checkHealth, 10000);
    return () => clearInterval(id);
  }, [checkHealth]);

  // Scroll chat to last message and keep focus on input
  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, loading]);
  useEffect(() => {
    if (tab === "chat") chatInputRef.current?.focus();
  }, [tab]);

  const fetchRouterMetrics = useCallback(async () => {
    setRouterLoading(true);
    setRouterError(null);
    try {
      const data = await invoke<RouterMetrics>("get_router_metrics", {
        port: DAEMON_PORT,
      });
      setRouterMetrics(data as RouterMetrics);
    } catch (e) {
      setRouterError(String(e));
      setRouterMetrics(null);
    } finally {
      setRouterLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab === "router") fetchRouterMetrics();
  }, [tab, fetchRouterMetrics]);

  const fetchDocs = useCallback(async () => {
    setDocLoading(true);
    setDocError(null);
    try {
      const content = await invoke<string>("get_docs", { port: DAEMON_PORT });
      setDocContent(content);
    } catch (e) {
      setDocError(String(e));
      setDocContent(null);
    } finally {
      setDocLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab === "docs") fetchDocs();
  }, [tab, fetchDocs]);

  const fetchActivityTasks = useCallback(async () => {
    setActivityLoading(true);
    try {
      const data = await invoke<{ tasks?: Array<{ id?: string; status?: string }> }>("get_tasks", {
        port: DAEMON_PORT,
      });
      const list = data?.tasks ?? [];
      const tasks = list
        .map((t) => ({ id: t.id ?? "", status: t.status ?? "?" }))
        .filter((t) => t.id);
      setActivityTasks(tasks);
      setActivitySelected((prev) => (prev >= tasks.length && tasks.length > 0 ? tasks.length - 1 : prev));
    } catch {
      setActivityTasks([]);
    } finally {
      setActivityLoading(false);
    }
  }, []);

  const fetchActivityEvents = useCallback(async (taskId: string) => {
    try {
      const data = await invoke<{ events?: Array<{ event_type?: string; payload?: unknown; at?: string }> }>(
        "get_task_events",
        { taskId, port: DAEMON_PORT }
      );
      const list = data?.events ?? [];
      setActivityEvents(
        list.map((e) => ({
          event_type: e.event_type ?? "?",
          payload: e.payload,
          at: e.at ?? "",
        }))
      );
    } catch {
      setActivityEvents([]);
    }
  }, []);

  useEffect(() => {
    if (tab === "activity") fetchActivityTasks();
  }, [tab, fetchActivityTasks]);

  useEffect(() => {
    const task = activityTasks[activitySelected];
    if (task?.id) fetchActivityEvents(task.id);
    else setActivityEvents([]);
  }, [activityTasks, activitySelected, fetchActivityEvents]);

  const runSlashCommand = async (input: string): Promise<string> => {
    const parts = input.replace(/^\//, "").trim().split(/\s+/);
    const cmd = parts[0]?.toLowerCase() ?? "";
    const port = DAEMON_PORT;

    if (cmd === "help" || cmd === "?") {
      return `Commandes disponibles:
/help, /?         — cette aide
/status           — état du daemon
/doctor           — diagnostic (daemon, ollama, vault, spec)
/advice           — conseil diagnostic (RAG + modèle)
/metrics          — métriques du routeur LLM
/models           — liste des modèles (tous les providers)
/config list      — variables (akasha.env)
/config get KEY   — valeur d'une variable
/config set K V   — définir variable
/vault list       — clés du vault (noms uniquement)
/plugins          — liste des plugins
/reload           — recharger les plugins
/restart          — redémarrer le daemon
/vault set        — utiliser le CLI : akasha vault set KEY [value]`;
    }
    if (cmd === "status") {
      const r = await invoke<{ ok: boolean }>("check_health", { port });
      return r?.ok ? "Daemon : OK" : "Daemon : déconnecté ou erreur";
    }
    if (cmd === "doctor") {
      const doc = await invoke<{ ok: boolean; checks: Array<{ id: string; ok: boolean; description: string }> }>("get_doctor", { port });
      if (!doc?.checks?.length) return "Impossible de récupérer le diagnostic.";
      const lines = doc.checks.map((c) => `  [${c.ok ? "OK" : "KO"}] ${c.id} — ${c.description}`);
      return `Doctor — diagnostic\n${lines.join("\n")}\n\n${doc.ok ? "Tous les checks sont OK." : "Certains checks ont échoué."}`;
    }
    if (cmd === "advice") {
      const doc = await invoke("get_doctor", { port });
      const adviceResp = await invoke<{ advice?: string; model_used?: string }>("get_advice", { health: doc, port });
      const advice = (adviceResp?.advice ?? "").trim();
      const model = adviceResp?.model_used ?? "?";
      if (!advice) return `(Aucun conseil retourné. Modèle utilisé : ${model}.)`;
      return `Conseil diagnostic (modèle: ${model})\n\n${advice}`;
    }
    if (cmd === "plugins") {
      const list = await invoke<Array<{ id?: string; name?: string; version?: string }>>("get_plugins", { port });
      if (!list?.length) return "Aucun plugin installé.";
      return list.map((p) => `${p.id ?? "?"} — ${p.name ?? "?"} (${p.version ?? "?"})`).join("\n");
    }
    if (cmd === "reload") {
      await invoke("reload_plugins", { port });
      return "Plugins rechargés.";
    }
    if (cmd === "metrics") {
      const data = await invoke<Record<string, ModelMetricsEntry>>("get_router_metrics", { port });
      if (!data || Object.keys(data).length === 0) return "Aucune métrique.";
      return Object.entries(data)
        .map(
          ([model, m]) =>
            `${model} : ${m.total_requests} requêtes (${m.successful_requests} ok, ${m.failed_requests} échecs), ${m.total_latency_ms} ms`
        )
        .join("\n");
    }
    if (cmd === "models") {
      const providers = await invoke<Record<string, string[]>>("get_router_models", { port });
      if (!providers || Object.keys(providers).length === 0) return "Aucun modèle configuré.";
      const lines: string[] = [];
      for (const [provider, models] of Object.entries(providers)) {
        if (models?.length) {
          lines.push(`${provider}:`);
          lines.push(...models.map((m) => `  ${m}`));
        }
      }
      return lines.length ? lines.join("\n") : "Aucun modèle listé.";
    }
    if (cmd === "config") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "list") {
        const vars = await invoke<Record<string, string>>("get_config", { port });
        if (!vars || Object.keys(vars).length === 0) return "Aucune variable.";
        return Object.entries(vars)
          .map(([k, v]) => `${k}=${v}`)
          .join("\n");
      }
      if (sub === "get") {
        const key = parts[2];
        if (!key) return "Usage: /config get KEY";
        const vars = await invoke<Record<string, string>>("get_config", { port });
        const val = vars?.[key];
        return val ?? "Clé non trouvée.";
      }
      if (sub === "set") {
        const key = parts[2];
        const value = parts.slice(3).join(" ") || "";
        if (!key) return "Usage: /config set KEY value";
        await invoke("set_config", { key, value, port });
        return `Variable ${key} définie.`;
      }
      return "Usage: /config list | get KEY | set KEY value";
    }
    if (cmd === "vault") {
      const sub = parts[1]?.toLowerCase() ?? "";
      if (sub === "list") {
        const keys = await invoke<string[]>("get_vault_keys", { port });
        return keys?.length ? keys.join("\n") : "Vault vide.";
      }
      if (sub === "set") return "Utilisez le CLI : akasha vault set KEY [value]";
      return "Usage: /vault list";
    }
    if (cmd === "restart") {
      await invoke("restart_daemon", { port });
      return "Redémarrage demandé (le daemon va se fermer).";
    }
    if (!cmd) return "Tapez /help pour les commandes.";
    return `Commande inconnue: /${cmd}. Tapez /help.`;
  };

  const handleSend = async () => {
    if (!message.trim() || loading) return;

    const userMessage = message.trim();
    setMessages((prev) => [...prev, { role: "user", text: userMessage }]);
    setMessage("");
    setLoading(true);
    chatInputRef.current?.focus();

    try {
      if (userMessage.startsWith("/")) {
        const result = await runSlashCommand(userMessage);
        setMessages((prev) => [...prev, { role: "system", text: result }]);
      } else {
        const result = await invoke<{ reply: string; session_id: string }>("send_message", {
          message: userMessage,
          session_id: sessionId,
          port: DAEMON_PORT,
        });
        if (result?.session_id) setSessionId(result.session_id);
        setMessages((prev) => [
          ...prev,
          { role: "assistant", text: result?.reply ?? "Done." },
        ]);
      }
    } catch (err) {
      setMessages((prev) => [
        ...prev,
        {
          role: "assistant",
          text: `Erreur : ${String(err)}`,
          error: true,
        },
      ]);
    } finally {
      setLoading(false);
      chatInputRef.current?.focus();
    }
  };

  return (
    <div className="app">
      <header className="header">
        <h1 className="logo">Akasha</h1>
        <p className="tagline">Local-first AI assistant</p>
        <div className="daemon-status" role="status" aria-live="polite">
          <span
            className={`status-dot ${health?.ok ? "connected" : "disconnected"}`}
            aria-hidden
          />
          {health?.ok ? (
            <span>Daemon connecté (port {health.port ?? DAEMON_PORT})</span>
          ) : (
            <span>Daemon déconnecté — lancez <code>akasha start</code></span>
          )}
        </div>
        <nav className="tabs" role="tablist" aria-label="Sections">
          <button
            role="tab"
            aria-selected={tab === "chat"}
            aria-controls="panel-chat"
            id="tab-chat"
            className={tab === "chat" ? "active" : ""}
            onClick={() => setTab("chat")}
          >
            Chat
          </button>
          <button
            role="tab"
            aria-selected={tab === "router"}
            aria-controls="panel-router"
            id="tab-router"
            className={tab === "router" ? "active" : ""}
            onClick={() => setTab("router")}
          >
            Routeur
          </button>
          <button
            role="tab"
            aria-selected={tab === "docs"}
            aria-controls="panel-docs"
            id="tab-docs"
            className={tab === "docs" ? "active" : ""}
            onClick={() => setTab("docs")}
          >
            Documentation
          </button>
          <button
            role="tab"
            aria-selected={tab === "activity"}
            aria-controls="panel-activity"
            id="tab-activity"
            className={tab === "activity" ? "active" : ""}
            onClick={() => setTab("activity")}
          >
            Activité
          </button>
          <button
            role="tab"
            aria-selected={tab === "settings"}
            aria-controls="panel-settings"
            id="tab-settings"
            className={tab === "settings" ? "active" : ""}
            onClick={() => setTab("settings")}
          >
            Paramètres
          </button>
        </nav>
      </header>

      <main className="main">
        {tab === "chat" && (
          <section
            id="panel-chat"
            role="tabpanel"
            aria-labelledby="tab-chat"
            className="panel chat-panel"
          >
            <div className="chat-area">
              {messages.length === 0 ? (
                <div className="chat-placeholder">
                  <p>Écrivez un message pour commencer.</p>
                  <p>
                    Commandes <code>/</code> : <code>/help</code> pour l’aide, <code>/status</code>, <code>/config list</code>, etc.
                  </p>
                  <p>
                    Daemon : <code>akasha start</code>
                  </p>
                </div>
              ) : (
                messages.map((m, i) => (
                  <div
                    key={i}
                    className={`message ${m.role} ${m.error ? "error" : ""}`}
                  >
                    <span className="role" aria-hidden>
                      {m.role === "user" ? "Vous" : m.role === "system" ? "Système" : "Akasha"}
                    </span>
                    <div className="text" style={{ whiteSpace: "pre-wrap" }}>
                      {m.text}
                    </div>
                  </div>
                ))
              )}
              {loading && (
                <div className="message assistant loading" aria-busy="true">
                  <span className="role">Akasha</span>
                  <span className="loading-dots">Réflexion…</span>
                </div>
              )}
              <div ref={chatEndRef} aria-hidden />
            </div>
            <div className="input-area">
              <label htmlFor="chat-input" className="sr-only">
                Votre message
              </label>
              <input
                ref={chatInputRef}
                id="chat-input"
                type="text"
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleSend()}
                placeholder="Votre message…"
                disabled={loading}
                aria-describedby="send-hint"
              />
              <button
                onClick={handleSend}
                disabled={loading || !message.trim()}
                aria-label="Envoyer le message"
              >
                Envoyer
              </button>
            </div>
            <p id="send-hint" className="hint sr-only">
              Entrée pour envoyer
            </p>
          </section>
        )}

        {tab === "router" && (
          <section
            id="panel-router"
            role="tabpanel"
            aria-labelledby="tab-router"
            className="panel router-panel"
          >
            <h2 className="panel-title">Métriques du routeur LLM</h2>
            {routerLoading && (
              <p className="loading-inline" aria-busy="true">
                Chargement…
              </p>
            )}
            {routerError && (
              <div className="error-banner" role="alert">
                {routerError}
              </div>
            )}
            {!routerLoading && !routerError && routerMetrics && (
              <>
                <button
                  type="button"
                  className="refresh-btn"
                  onClick={fetchRouterMetrics}
                  aria-label="Rafraîchir les métriques"
                >
                  Rafraîchir
                </button>
                {Object.keys(routerMetrics).length === 0 ? (
                  <p className="empty-state">
                    Aucune requête enregistrée. Envoyez un message dans le Chat
                    pour générer des métriques.
                  </p>
                ) : (
                  <div className="metrics-table-wrap">
                    <table className="metrics-table" role="table">
                      <thead>
                        <tr>
                          <th scope="col">Modèle</th>
                          <th scope="col">Requêtes</th>
                          <th scope="col">Réussies</th>
                          <th scope="col">Échecs</th>
                          <th scope="col">Latence (ms)</th>
                          <th scope="col">Tokens</th>
                          <th scope="col">Fallbacks</th>
                        </tr>
                      </thead>
                      <tbody>
                        {Object.entries(routerMetrics).map(([key, m]) => (
                          <tr key={key}>
                            <td data-label="Modèle">{key}</td>
                            <td data-label="Requêtes">{m.total_requests}</td>
                            <td data-label="Réussies">
                              {m.successful_requests}
                            </td>
                            <td data-label="Échecs">{m.failed_requests}</td>
                            <td data-label="Latence (ms)">
                              {m.total_latency_ms}
                            </td>
                            <td data-label="Tokens">{m.total_tokens}</td>
                            <td data-label="Fallbacks">
                              {m.fallback_triggered} / {m.fallback_success}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </>
            )}
          </section>
        )}

        {tab === "docs" && (
          <section
            id="panel-docs"
            role="tabpanel"
            aria-labelledby="tab-docs"
            className="panel docs-panel"
          >
            <h2 className="panel-title">Documentation utilisateur</h2>
            {docLoading && (
              <p className="loading-inline" aria-busy="true">
                Chargement…
              </p>
            )}
            {docError && (
              <div className="error-banner" role="alert">
                {docError}
                <p>Assurez-vous que le daemon est démarré (<code>akasha start</code>).</p>
              </div>
            )}
            {!docLoading && !docError && docContent && (
              <>
                <button
                  type="button"
                  className="refresh-btn"
                  onClick={fetchDocs}
                  aria-label="Rafraîchir la documentation"
                >
                  Rafraîchir
                </button>
                <div className="doc-content doc-markdown">
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>
                    {docContent}
                  </ReactMarkdown>
                </div>
              </>
            )}
          </section>
        )}

        {tab === "activity" && (
          <section
            id="panel-activity"
            role="tabpanel"
            aria-labelledby="tab-activity"
            className="panel activity-panel"
          >
            <h2 className="panel-title">Activité (tâches et événements)</h2>
            <button
              type="button"
              className="refresh-btn"
              onClick={fetchActivityTasks}
              aria-label="Rafraîchir l’activité"
              disabled={activityLoading}
            >
              Rafraîchir
            </button>
            {activityLoading && (
              <p className="loading-inline" aria-busy="true">
                Chargement…
              </p>
            )}
            {!activityLoading && (
              <>
                <h3>Tâches</h3>
                {activityTasks.length === 0 ? (
                  <p className="empty-state">Aucune tâche. Envoyez un message dans le Chat.</p>
                ) : (
                  <ul className="activity-task-list" role="list">
                    {activityTasks.map((t, i) => (
                      <li
                        key={t.id}
                        className={i === activitySelected ? "selected" : ""}
                        role="button"
                        tabIndex={0}
                        onClick={() => setActivitySelected(i)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            setActivitySelected(i);
                          }
                          if (e.key === "ArrowDown" && i < activityTasks.length - 1)
                            setActivitySelected(i + 1);
                          if (e.key === "ArrowUp" && i > 0) setActivitySelected(i - 1);
                        }}
                      >
                        <span className="task-id">{t.id.slice(-8)}</span>{" "}
                        <span className="task-status">{t.status}</span>
                      </li>
                    ))}
                  </ul>
                )}
                <h3>Événements</h3>
                {activityEvents.length === 0 ? (
                  <p className="empty-state">
                    {activityTasks.length > 0 ? "Aucun événement pour cette tâche." : "Sélectionnez une tâche."}
                  </p>
                ) : (
                  <ul className="activity-events-list" role="list">
                    {activityEvents.map((e, i) => (
                      <li key={i}>
                        <strong>{eventTypeLabel(e.event_type)}</strong> @ {e.at}
                        {e.payload != null && (
                          <pre className="event-payload">{JSON.stringify(e.payload, null, 2)}</pre>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </>
            )}
          </section>
        )}

        {tab === "settings" && (
          <section
            id="panel-settings"
            role="tabpanel"
            aria-labelledby="tab-settings"
            className="panel settings-panel"
          >
            <h2 className="panel-title">Paramètres</h2>
            <dl className="settings-list">
              <dt>Port du daemon</dt>
              <dd>
                <code>{DAEMON_PORT}</code> (défaut)
              </dd>
              <dt>Répertoire de données</dt>
              <dd>
                <code>%LOCALAPPDATA%\akasha</code> (Windows) ou{" "}
                <code>~/.local/share/akasha</code> (Linux/macOS)
              </dd>
            </dl>
            <p className="settings-doc">
              Configuration : variables d’environnement <code>AKASHA_*</code>,{" "}
              <code>OLLAMA_HOST</code>. Voir l’onglet Documentation pour le guide complet.
            </p>
          </section>
        )}
      </main>
    </div>
  );
}

export default App;
