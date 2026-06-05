import { describe, expect, it } from "vitest";
import {
  buildCookbookPricingLookup,
  buildModelUsageDisplay,
  estimateCostFromRates,
  parseUsageFromTaskStatus,
  resolveTaskUsage,
} from "./modelUsage";

describe("parseUsageFromTaskStatus", () => {
  it("reads last turn fields", () => {
    expect(
      parseUsageFromTaskStatus({
        last_turn_tokens_in: 120,
        last_turn_tokens_out: 45,
        last_turn_cost_usd: 0.002,
        last_turn_latency_ms: 1500,
        last_turn_model_used: "ollama/qwen3.5:9b",
      }),
    ).toEqual({
      model: "ollama/qwen3.5:9b",
      promptTokens: 120,
      completionTokens: 45,
      costUsd: 0.002,
      latencyMs: 1500,
    });
  });
});

describe("resolveTaskUsage", () => {
  it("falls back to task_completed event when last_turn stats are empty", () => {
    expect(
      resolveTaskUsage(
        { last_turn_tokens_in: 0, last_turn_tokens_out: 0 },
        [
          {
            event_type: "task_completed",
            payload: {
              prompt_tokens: 88,
              completion_tokens: 42,
              cost_usd: 0.0012,
              model_used: "ollama/qwen3.5:9b",
            },
          },
        ],
      ),
    ).toEqual({
      model: "ollama/qwen3.5:9b",
      promptTokens: 88,
      completionTokens: 42,
      costUsd: 0.0012,
      latencyMs: undefined,
    });
  });
});

describe("buildModelUsageDisplay", () => {
  it("estimates split cost from cookbook rates when provider cost is zero", () => {
    const display = buildModelUsageDisplay({
      promptTokens: 1_000_000,
      completionTokens: 500_000,
      priceRates: { inputPerMillion: 2, outputPerMillion: 4 },
    });
    expect(display?.costEstimated).toBe(true);
    expect(display?.costInputUsd).toBeCloseTo(2);
    expect(display?.costOutputUsd).toBeCloseTo(2);
    expect(display?.costUsd).toBeCloseTo(4);
  });
});

describe("buildCookbookPricingLookup", () => {
  it("indexes by provider/model and bare model id", () => {
    const map = buildCookbookPricingLookup([
      {
        provider: "openrouter",
        model: "qwen/qwen3.5-9b",
        price_input_per_million: 0.1,
        price_output_per_million: 0.2,
      },
    ]);
    expect(map.get("openrouter/qwen/qwen3.5-9b")?.inputPerMillion).toBe(0.1);
    expect(map.get("qwen/qwen3.5-9b")?.outputPerMillion).toBe(0.2);
  });
});

describe("estimateCostFromRates", () => {
  it("returns null when no rates", () => {
    expect(estimateCostFromRates(100, 100, {})).toBeNull();
  });
});
