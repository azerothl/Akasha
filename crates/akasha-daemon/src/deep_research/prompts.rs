//! Prompt templates for IterResearch-style deep research (rigorous, evidence-first).

pub const RESEARCH_PLAN_PROMPT: &str = r#"You are a research strategist. Before searching, analyze this question and create a research plan.

**Question:** {question}

Break this question down:
1. What are the key sub-topics that need to be covered for a comprehensive answer?
2. What specific data points, facts, or perspectives should we look for?
3. What would a complete, high-quality answer include?
4. Which **source types** are required for a global view (encyclopedia, academic papers, news, forums, official docs, code repos, industry reports)?
5. What claims would be **unsupported** without primary sources?

Plan for **depth and verifiability**, not surface-level summaries. Identify gaps that generic search snippets cannot fill.

Return a JSON object with:
- "sub_questions": Array of 6-10 specific sub-questions to investigate
- "key_topics": Array of key topics/angles to cover
- "source_types": Array of source categories to consult (e.g. wikipedia, arxiv, reddit, github, news, scientific_journal, official_docs)
- "success_criteria": One sentence describing what a complete answer looks like (must mention diverse sources and evidence)

Example:
{{
 "sub_questions": ["What is the cost of living in X?", "How is the healthcare system?"],
 "key_topics": ["economy", "healthcare", "safety", "culture"],
 "source_types": ["wikipedia", "news", "reddit", "government_stats"],
 "success_criteria": "A balanced comparison with at least 12 distinct web sources across wiki, news, and academic categories, each major claim cited."
}}"#;

pub const QUERY_GEN_PROMPT: &str = r#"You are a research assistant planning web searches for **deep**, multi-source investigation.

**Original question:** {question}

**Research plan:**
{research_plan}

**What we know so far:**
{report}

**Round:** {round_num}
**Pages gathered so far:** {pages_gathered}

Generate {num_queries} search queries for this round.
{round_instruction}

Rules:
- Do NOT only rephrase the main question — target **specific sub-questions** and evidence types.
- Prefer queries that surface **authoritative and diverse** sources: use `site:` operators when helpful (e.g. site:en.wikipedia.org, site:arxiv.org, site:github.com, site:reddit.com, site:reuters.com, site:nature.com, site:stackoverflow.com, site:docs.*).
- Include at least one query aimed at **data, statistics, charts, or benchmarks** when relevant.
- Include at least one query for **expert / academic** material and one for **practitioner / community** perspectives (forums, GitHub issues, Reddit).
- If pages gathered is low, prioritize **breadth** (new domains) over repeating similar queries.
- Avoid duplicate queries already used; go deeper into under-covered angles shown in the report.
- Use the current year when freshness matters.

Return ONLY a JSON array of query strings, nothing else.
Example: ["site:arxiv.org topic survey 2025", "topic benchmark comparison data", "site:reddit.com topic experience"]"#;

pub const SYNTHESIZE_PROMPT: &str = r#"You are updating an evolving research report (working draft).

**Original question:** {question}

**Current report:**
{report}

**New findings from this round:**
{new_findings}

Integrate the new findings into the existing report. Produce an updated, well-organized draft that:
- Answers sub-questions from the research plan progressively
- Preserves **specific facts, numbers, quotes, and URLs** from findings — do not over-summarize into vagueness
- Notes agreements and **disagreements** between sources; label single-source claims as such
- Flags remaining gaps to investigate in later rounds
- Uses inline markdown links [label](url) for every important claim tied to a source
- Uses neutral, analytical tone — no marketing hype or unsupported superlatives

Write only the updated report — no preamble, greeting, or meta-commentary."#;

pub const STOP_PROMPT: &str = r#"You are deciding whether a research report is comprehensive enough to stop gathering more sources.

**Original question:** {question}

**Current report:**
{report}

**Rounds completed:** {round_num}
**Unique pages read:** {source_count}
**Unique domains:** {unique_domains}

Reply YES **only if ALL** are true:
- Major sub-questions from the plan are addressed with **evidence** (not speculation or generic LLM knowledge)
- Multiple **source types** are represented (not a single blog or forum)
- At least {min_sources} unique pages worth of material is reflected in the draft
- At least {min_domains} distinct domains (sites) contributed evidence
- No critical gap remains that another search round would likely fill

Otherwise reply NO.

Reply with ONLY "YES" or "NO" followed by a brief one-sentence reason.
Example: "NO — Only 4 pages and 2 domains; missing academic and official documentation."
Example: "YES — 20+ pages across wiki, arxiv, news, and github with plan covered.""#;

pub const FINAL_REPORT_PROMPT: &str = r#"Write a **rigorous, evidence-based** research report answering this question.

**Question:** {question}

**Source manifest (pages read — cite these URLs for claims):**
{source_manifest}

**All collected evidence and synthesis (working draft):**
{report}

**Quality requirements:**
- Target **{min_words}+ words** when the evidence supports it; if evidence is thin, write a shorter honest report and expand ## Limitations instead of padding
- Do NOT use the user's raw question as the document title (a professional cover title is generated separately)
- Do NOT open with a personal greeting (no "Bonjour", "Hello", etc.)
- One executive summary section at the top (## Executive summary OR ## Résumé exécutif — match the report language, **never both**)
- Use clear ## and ### headings; multiple paragraphs per section
- **Synthesize and analyze** — explain mechanisms, trade-offs, context; compare viewpoints
- Include **specific data points**, statistics, dates, and named entities **only when they appear in the evidence**
- Every major claim MUST have an inline citation [source title](url) from the source manifest
- Mark uncertain or single-source claims explicitly (e.g. "According to one blog post…", "Evidence is mixed…")
- Include ## Sources and methodology — list key references and briefly describe how sources were selected
- Include ## Limitations and confidence — gaps, conflicts between sources, what was not verified
- Where comparisons or flows help understanding, add a **mermaid** diagram in a fenced ```mermaid block
- Include **2-4 illustrative images** only when https URLs exist in the evidence
- Add tables for comparisons when appropriate
- End with a ## Conclusion that directly answers the question and states confidence level (high / medium / low)
- Tone: **professional, neutral, precise** — like an analyst briefing or technical white paper. Avoid hype ("revolutionary", "decisive leap", "major competitive advantage") unless a cited source uses that language in quotes.
- Do NOT invent product features, paper titles, statistics, or codebase details absent from the evidence.
{category_override}"#;

pub const FINAL_REPORT_RETRY_PROMPT: &str = r#"Revise this research report to meet quality standards.

**Question:** {question}

**Source manifest:**
{source_manifest}

**Current report (needs improvement):**
{report}

**Issues to fix:**
{quality_issues}

Requirements:
- Add missing inline [title](url) citations from the source manifest for major claims
- Expand thin sections using only evidence from the draft and manifest
- Keep ## Limitations and confidence section
- Neutral analytical tone; no greeting; no hype
- Same language as the current report

Output the full revised markdown report only."#;

pub const EXTRACTOR_PROMPT: &str = r#"Extract information from this webpage relevant to the research goal.

**Research goal:** {goal}

**Webpage content:**
{webpage_content}

Return a JSON object with:
- "summary": 3-5 sentence summary of relevant content (neutral tone)
- "evidence": Key facts, quotes, statistics, or data points (max 3000 chars) — preserve specifics verbatim when possible
- "image_urls": Array of up to 3 https image URLs mentioned or implied (charts, diagrams, og:image) — empty array if none
- "rational": Why this page is or isn't useful for the goal

If the page is irrelevant or empty, set summary to "LOW_QUALITY: ..." explaining why."#;

pub const REPORT_TITLE_PROMPT: &str = r#"Write a concise, professional **report title** for a deep research document.

**User question (for context only — do NOT copy as the title):**
{question}

**Category:** {category}
**Report excerpt (optional):**
{excerpt}

Rules:
- 6–14 words, declarative (not a question, no trailing "?")
- Same language as the user question
- Specific and informative (like a technical white-paper headline)
- No quotation marks around the title

Reply with ONLY the title text, nothing else."#;

pub const CATEGORY_CLASSIFY_PROMPT: &str = r#"Classify this research question into exactly ONE category.
Categories:
- product — buying, pricing, reviews of products/tools
- comparison — comparing options (A vs B, trade-offs)
- howto — step-by-step implementation guide
- factcheck — verifying a specific claim
- integration — how something fits into an existing system/product/codebase (e.g. "integrate X into Y", "compare with current architecture")
- general — everything else

If none fit well, respond with: general

Question: {question}

Respond with ONLY the category name, nothing else."#;

pub fn category_prompt_override(category: &str) -> &'static str {
    match category {
        "product" => PRODUCT_CATEGORY,
        "comparison" => COMPARISON_CATEGORY,
        "howto" => HOWTO_CATEGORY,
        "factcheck" => FACTCHECK_CATEGORY,
        "integration" => INTEGRATION_CATEGORY,
        _ => GENERAL_DEPTH_CATEGORY,
    }
}

const GENERAL_DEPTH_CATEGORY: &str = r#"
- Aim for encyclopedic depth: history/context, current state, stakeholders, data, risks, and outlook
- Include at least one mermaid diagram if relationships or process flows are non-trivial
- Separate **established consensus** from **debated** or **single-source** claims"#;

const INTEGRATION_CATEGORY: &str = r#"
IMPORTANT FORMAT OVERRIDE — integration / architecture analysis:
- Include ## Current state (what the evidence says exists today — do not invent codebase details)
- Include ## Gap analysis (what is missing vs what the question asks for)
- Include ## Recommendations by priority (P0/P1/P2) with effort/risk notes
- If comparing to a named product (e.g. Akasha), label inferences clearly vs cited external facts
- Use a markdown table: Technique | Evidence strength | Fit | Implementation effort
- End with ## Open questions that need primary sources or code inspection"#;

const PRODUCT_CATEGORY: &str = r#"
IMPORTANT FORMAT OVERRIDE — this is a PRODUCT research report:
- Structure as a RANKED LIST of products/options (best first)
- For EACH product include: name as ### heading, approximate price, 2-3 sentence summary, **Pros:** bullet list, **Cons:** bullet list, **Where to buy:** URLs as links
- Start with a quick-compare markdown table of top picks (columns: Name, Price, Best For, Rating)
- Add a mermaid quadrant or flowchart if it clarifies positioning
- Include product images when URLs exist in evidence
- End with a ## Verdict section picking Best Overall and Best Value
- Still include source citations inline"#;

const COMPARISON_CATEGORY: &str = r#"
IMPORTANT FORMAT OVERRIDE — this is a COMPARISON report:
- Create a ## Comparison Table as a markdown table comparing ALL options across key criteria
- Add a ```mermaid diagram (e.g. quadrantChart or flowchart) for trade-offs
- Write a ## section per option with strengths, weaknesses, and ideal use case
- End with ## Best For verdicts
- Include a ## Shared Considerations section"#;

const HOWTO_CATEGORY: &str = r#"
IMPORTANT FORMAT OVERRIDE — this is a HOW-TO guide:
- Start with ## Quick Guide — numbered list (one line per step)
- Then ## Prerequisites
- Then ## Step 1:, ## Step 2:, etc. with detailed instructions
- Cite GitHub repos, official docs, and Stack Overflow threads from evidence
- Use blockquotes for tips: > **Tip:** ...
- Add a mermaid flowchart of the overall process when steps branch
- End with ## Common Mistakes"#;

const FACTCHECK_CATEGORY: &str = r#"
IMPORTANT FORMAT OVERRIDE — this is a FACT-CHECK report:
- Start with ## The Claim
- Create ## Evidence For and ## Evidence Against sections with primary-source links
- Include a ## Verdict: **Supported**, **Mixed Evidence**, or **Unsupported**
- End with ## Nuance & Caveats"#;

pub fn current_date_context() -> String {
    let now = chrono::Local::now();
    format!(
        "Today's date is {} ({}). When a search query needs a year or refers to 'latest'/'current'/'this year', use {} or relative wording — never a year inferred from training data.\n\n",
        now.format("%B %d, %Y"),
        now.format("%Y-%m-%d"),
        now.format("%Y")
    )
}
