import { describe, expect, it } from "vitest";
import {
  buildRecipePrompt,
  categoryLabel,
  defaultVariableValues,
  filterRecipeSummaries,
  substitutePromptTemplate,
  type RecipeSummary,
} from "./cookbookRecipes";

describe("cookbookRecipes", () => {
  it("substitutes prompt template variables", () => {
    const out = substitutePromptTemplate("Hello {name}, task: {task}", { name: "Ada", task: "test" });
    expect(out).toBe("Hello Ada, task: test");
  });

  it("builds recipe prompt from template and defaults", () => {
    const prompt = buildRecipePrompt(
      {
        id: "x",
        category: "prompts",
        tags: [],
        difficulty: "beginner",
        last_reviewed: "2026-06-08",
        locales: { en: { title: "T", summary: "S", steps_md: "" }, fr: { title: "T", summary: "S", steps_md: "" } },
        prompt_template: "Do {task}",
        variables: [{ key: "task", label_en: "Task", label_fr: "Tâche", default: "something" }],
      },
      defaultVariableValues({
        id: "x",
        category: "prompts",
        tags: [],
        difficulty: "beginner",
        last_reviewed: "2026-06-08",
        locales: { en: { title: "T", summary: "S", steps_md: "" }, fr: { title: "T", summary: "S", steps_md: "" } },
        prompt_template: "Do {task}",
        variables: [{ key: "task", label_en: "Task", label_fr: "Tâche", default: "something" }],
      }),
    );
    expect(prompt).toBe("Do something");
  });

  it("filters summaries by category and search", () => {
    const items: RecipeSummary[] = [
      {
        id: "a",
        category: "rag",
        tags: ["memory"],
        difficulty: "beginner",
        last_reviewed: "2026-06-08",
        title: "User RAG",
        summary: "Index documents",
      },
      {
        id: "b",
        category: "evaluation",
        tags: ["compare"],
        difficulty: "beginner",
        last_reviewed: "2026-06-08",
        title: "Blind compare",
        summary: "Compare models",
      },
    ];
    expect(filterRecipeSummaries(items, "rag", "").length).toBe(1);
    expect(filterRecipeSummaries(items, "all", "blind").length).toBe(1);
  });

  it("localizes category labels", () => {
    expect(categoryLabel("fine_tuning", "en")).toBe("Fine-tuning");
    expect(categoryLabel("fine_tuning", "fr")).toBe("Fine-tuning");
  });
});
