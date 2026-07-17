import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const DAEMON_PORT = 3876;

type MetricsEntry = {
  total_requests?: number;
  successful_requests?: number;
  failed_requests?: number;
  total_tokens?: number;
  total_cost_usd?: number;
  total_latency_ms?: number;
};

type Period = "week" | "month";

type Props = {
  t: (key: string) => string;
  locale: "fr" | "en";
};

export function UsageDashboardPanel({ t, locale }: Props) {
  const [period, setPeriod] = useState<Period>("week");
  const [metrics, setMetrics] = useState<Record<string, MetricsEntry>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await invoke<Record<string, MetricsEntry>>("get_router_metrics", {
        port: DAEMON_PORT,
        period,
      });
      setMetrics(data ?? {});
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setMetrics({});
    } finally {
      setLoading(false);
    }
  }, [period]);

  useEffect(() => {
    void load();
  }, [load]);

  const rows = useMemo(() => {
    return Object.entries(metrics)
      .map(([key, m]) => ({
        key,
        requests: m.total_requests ?? 0,
        ok: m.successful_requests ?? 0,
        fail: m.failed_requests ?? 0,
        tokens: m.total_tokens ?? 0,
        cost: m.total_cost_usd ?? 0,
        latency: m.total_latency_ms ?? 0,
      }))
      .sort((a, b) => b.tokens - a.tokens || b.requests - a.requests);
  }, [metrics]);

  const totals = useMemo(() => {
    return rows.reduce(
      (acc, r) => ({
        requests: acc.requests + r.requests,
        tokens: acc.tokens + r.tokens,
        cost: acc.cost + r.cost,
      }),
      { requests: 0, tokens: 0, cost: 0 },
    );
  }, [rows]);

  const en = locale === "en";
  const periodLabel = period === "week" ? (en ? "7 days" : "7 jours") : en ? "30 days" : "30 jours";

  return (
    <section className="settings-card">
      <h4>{t("settings.usage_title")}</h4>
      <p className="settings-doc muted">{t("settings.usage_hint")}</p>

      <div className="settings-row-actions">
        <label htmlFor="usage-period">
          {en ? "Period" : "Période"}
          <select
            id="usage-period"
            value={period}
            onChange={(e) => setPeriod(e.target.value as Period)}
            disabled={loading}
          >
            <option value="week">{en ? "7 days" : "7 jours"}</option>
            <option value="month">{en ? "30 days" : "30 jours"}</option>
          </select>
        </label>
        <button type="button" className="btn-secondary" disabled={loading} onClick={() => void load()}>
          {t("settings.embedded_refresh")}
        </button>
      </div>

      {error ? <p className="steering-queue-error">{error}</p> : null}
      {loading ? <p className="settings-doc muted">{t("common.loading")}</p> : null}

      {!loading && (
        <>
          <dl className="settings-list">
            <dt>{en ? "Period" : "Période"}</dt>
            <dd>{periodLabel}</dd>
            <dt>{en ? "Requests" : "Requêtes"}</dt>
            <dd>{totals.requests}</dd>
            <dt>{en ? "Tokens" : "Tokens"}</dt>
            <dd>{totals.tokens.toLocaleString()}</dd>
            <dt>{en ? "Est. cost (USD)" : "Coût estimé (USD)"}</dt>
            <dd>{totals.cost > 0 ? totals.cost.toFixed(4) : "—"}</dd>
          </dl>

          {rows.length === 0 ? (
            <p className="settings-doc muted">{t("settings.usage_empty")}</p>
          ) : (
            <table className="settings-table usage-table">
              <thead>
                <tr>
                  <th>{en ? "Model / provider" : "Modèle / provider"}</th>
                  <th>{en ? "Requests" : "Requêtes"}</th>
                  <th>Tokens</th>
                  <th>{en ? "Cost $" : "Coût $"}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <tr key={r.key}>
                    <td>
                      <code>{r.key}</code>
                    </td>
                    <td>
                      {r.requests}{" "}
                      <span className="muted">
                        ({r.ok}/{r.fail})
                      </span>
                    </td>
                    <td>{r.tokens.toLocaleString()}</td>
                    <td>{r.cost > 0 ? r.cost.toFixed(4) : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </section>
  );
}
