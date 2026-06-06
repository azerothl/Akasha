import { useMemo, useState } from "react";
import {
  buildModelUsageDisplay,
  formatLatency,
  formatUsd,
  type ModelUsageStats,
} from "../modelUsage";
import { useI18n } from "../useI18n";

type Props = {
  usage: ModelUsageStats;
  compact?: boolean;
  className?: string;
};

export function ModelUsageBadge({ usage, compact = false, className = "" }: Props) {
  const { t, locale } = useI18n();
  const en = locale === "en";
  const [open, setOpen] = useState(false);
  const display = useMemo(() => buildModelUsageDisplay(usage), [usage]);
  if (!display) return null;

  const summaryParts: string[] = [];
  if (display.promptTokens != null || display.completionTokens != null) {
    const inp = display.promptTokens ?? 0;
    const out = display.completionTokens ?? 0;
    summaryParts.push(`↑${inp.toLocaleString()} ↓${out.toLocaleString()}`);
  }
  if (display.latencyMs != null && display.latencyMs > 0) {
    summaryParts.push(formatLatency(display.latencyMs, locale));
  }
  if (display.costUsd != null && display.costUsd > 0) {
    summaryParts.push(formatUsd(display.costUsd, locale));
  }

  if (summaryParts.length === 0 && !display.model) return null;

  const summary = summaryParts.join(" · ");

  return (
    <div className={`model-usage-badge ${compact ? "model-usage-compact" : ""} ${className}`.trim()}>
      <button
        type="button"
        className="model-usage-toggle"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        title={en ? "Model usage details" : "Détails d'utilisation du modèle"}
      >
        <span className="model-usage-icon" aria-hidden>
          ⏱
        </span>
        <span className="model-usage-summary">{summary || (display.model ?? "—")}</span>
        <span className="model-usage-chevron" aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open ? (
        <dl className="model-usage-details">
          {display.model ? (
            <>
              <dt>{t("usage.model")}</dt>
              <dd>{display.model}</dd>
            </>
          ) : null}
          {display.promptTokens != null ? (
            <>
              <dt>{t("usage.tokens_in")}</dt>
              <dd>{display.promptTokens.toLocaleString()}</dd>
            </>
          ) : null}
          {display.completionTokens != null ? (
            <>
              <dt>{t("usage.tokens_out")}</dt>
              <dd>{display.completionTokens.toLocaleString()}</dd>
            </>
          ) : null}
          {display.latencyMs != null && display.latencyMs > 0 ? (
            <>
              <dt>{t("usage.latency")}</dt>
              <dd>{formatLatency(display.latencyMs, locale)}</dd>
            </>
          ) : null}
          {display.costInputUsd != null && display.costInputUsd > 0 ? (
            <>
              <dt>{display.costEstimated ? t("usage.cost_in_est") : t("usage.cost_in")}</dt>
              <dd>{formatUsd(display.costInputUsd, locale)}</dd>
            </>
          ) : null}
          {display.costOutputUsd != null && display.costOutputUsd > 0 ? (
            <>
              <dt>{display.costEstimated ? t("usage.cost_out_est") : t("usage.cost_out")}</dt>
              <dd>{formatUsd(display.costOutputUsd, locale)}</dd>
            </>
          ) : null}
          {display.costUsd != null && display.costUsd > 0 ? (
            <>
              <dt>{display.costEstimated ? t("usage.cost_total_est") : t("usage.cost_total")}</dt>
              <dd>{formatUsd(display.costUsd, locale)}</dd>
            </>
          ) : null}
        </dl>
      ) : null}
    </div>
  );
}
