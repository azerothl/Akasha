export type ResearchStepState = "pending" | "running" | "done" | "error";

export type ResearchPhase =
  | "idle"
  | "planning"
  | "searching"
  | "reading"
  | "analyzing"
  | "writing"
  | "steps"
  | "synthesizing"
  | "done";

export type ResearchSubAgentViz = {
  id: string;
  role: string;
  label: string;
  status: ResearchStepState;
};

export type ResearchBranchViz = {
  label: string;
  status: ResearchStepState;
  subAgents?: ResearchSubAgentViz[];
};

/** @deprecated use ResearchBranchViz */
export type Step = {
  label: string;
  status: ResearchStepState;
};

type Props = {
  branches: ResearchBranchViz[];
  /** Legacy flat steps — used when branches empty */
  steps?: Step[];
  phase: ResearchPhase;
  phaseLabel?: string;
  topic?: string;
};

const CX = 100;
const CY = 100;
const BRANCH_LEN = 68;
const SUB_BRANCH_LEN = 26;
const NODE_R = 7;
const SUB_NODE_R = 4;

function branchAngle(index: number, total: number): number {
  if (total <= 0) return -90;
  return -90 + (360 / total) * index;
}

function polar(cx: number, cy: number, r: number, deg: number) {
  const rad = (deg * Math.PI) / 180;
  return { x: cx + r * Math.cos(rad), y: cy + r * Math.sin(rad) };
}

function stepGlow(status: ResearchStepState, phase: ResearchPhase): number {
  if (status === "done") return 1;
  if (status === "running") return 0.75;
  if (status === "error") return 0.35;
  if (phase === "planning") return 0.12;
  return 0.22;
}

function subAgentOffset(index: number, total: number): number {
  if (total <= 1) return 0;
  const spread = 24;
  const mid = (total - 1) / 2;
  return (index - mid) * (spread / Math.max(total - 1, 1));
}

function roleClass(role: string): string {
  return `research-star-sub-${role.replace(/[^a-z0-9_-]/gi, "")}`;
}

export function ResearchStarViz({ branches, steps = [], phase, phaseLabel, topic }: Props) {
  const items: ResearchBranchViz[] =
    branches.length > 0
      ? branches
      : steps.map((s) => ({ label: s.label, status: s.status }));

  const n = items.length;
  const completed = items.filter((s) => s.status === "done").length;
  const coreGlow =
    phase === "done"
      ? 1
      : phase === "writing" || phase === "synthesizing"
        ? 0.85
        : phase === "planning"
          ? 0.45
          : phase === "searching" || phase === "reading" || phase === "analyzing"
            ? 0.55
            : 0.35 + (n > 0 ? (completed / n) * 0.5 : 0);

  return (
    <div className="research-star-wrap" aria-live="polite" aria-busy={phase !== "idle" && phase !== "done"}>
      <svg className="research-star-svg" viewBox="0 0 200 200" role="img" aria-hidden>
        <defs>
          <radialGradient id="research-star-core-glow" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="var(--research-star-glow)" stopOpacity={coreGlow} />
            <stop offset="100%" stopColor="var(--research-star-glow)" stopOpacity="0" />
          </radialGradient>
          <filter id="research-star-blur">
            <feGaussianBlur stdDeviation="2.5" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>

        <circle
          className={`research-star-halo ${phase === "planning" || phase === "writing" || phase === "synthesizing" ? "research-star-halo-pulse" : ""}`}
          cx={CX}
          cy={CY}
          r={28 + coreGlow * 10}
          fill="url(#research-star-core-glow)"
        />

        {items.map((branch, i) => {
          const angle = branchAngle(i, n);
          const end = polar(CX, CY, BRANCH_LEN, angle);
          const glow = stepGlow(branch.status, phase);
          const isActive = branch.status === "running";
          const subAgents = branch.subAgents ?? [];

          return (
            <g key={i} className={`research-star-branch research-star-branch-${branch.status}`}>
              <line
                x1={CX}
                y1={CY}
                x2={end.x}
                y2={end.y}
                className={isActive ? "research-star-ray-active" : ""}
                style={{ opacity: glow }}
              />
              {isActive ? (
                <line
                  x1={CX}
                  y1={CY}
                  x2={end.x}
                  y2={end.y}
                  className="research-star-ray-shimmer"
                  style={{ opacity: 0.5 + glow * 0.5 }}
                />
              ) : null}

              {subAgents.map((sub, si) => {
                const offset = subAgentOffset(si, subAgents.length);
                const subAngle = angle + offset;
                const subEnd = polar(end.x, end.y, SUB_BRANCH_LEN, subAngle);
                const subGlow = stepGlow(sub.status, phase);
                const subActive = sub.status === "running";
                return (
                  <g
                    key={sub.id}
                    className={`research-star-sub-branch ${roleClass(sub.role)} research-star-branch-${sub.status}`}
                  >
                    <line
                      x1={end.x}
                      y1={end.y}
                      x2={subEnd.x}
                      y2={subEnd.y}
                      className={subActive ? "research-star-ray-active" : "research-star-sub-ray"}
                      style={{ opacity: subGlow }}
                    />
                    <circle
                      cx={subEnd.x}
                      cy={subEnd.y}
                      r={SUB_NODE_R}
                      className={subActive ? "research-star-node-active" : ""}
                      style={{ opacity: Math.max(subGlow, 0.3) }}
                    />
                  </g>
                );
              })}

              <circle
                cx={end.x}
                cy={end.y}
                r={NODE_R + (branch.status === "done" ? 2 : 0)}
                className={isActive ? "research-star-node-active" : ""}
                style={{ opacity: Math.max(glow, 0.25) }}
                filter={branch.status === "done" || isActive ? "url(#research-star-blur)" : undefined}
              />
            </g>
          );
        })}

        <circle
          className={`research-star-core ${phase === "planning" || phase === "writing" || phase === "synthesizing" ? "research-star-core-pulse" : ""}`}
          cx={CX}
          cy={CY}
          r={14}
          style={{ opacity: 0.55 + coreGlow * 0.45 }}
        />
      </svg>

      {topic ? (
        <p className="research-star-topic" title={topic}>
          {topic}
        </p>
      ) : null}

      {phaseLabel ? <p className="research-star-phase muted">{phaseLabel}</p> : null}

      {n > 0 && phase !== "idle" ? (
        <p className="research-star-progress muted">
          {completed}/{n}
        </p>
      ) : null}
    </div>
  );
}
