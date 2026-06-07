import { marked } from "marked";
import TurndownService from "turndown";

const ASSET_MD_RE = /!\[([^\]]*)\]\((assets\/[^)]+)\)/g;

export function markdownToHtml(markdown: string): string {
  const parsed = marked.parse(markdown, { async: false });
  return typeof parsed === "string" ? parsed : "";
}

export function htmlToMarkdown(html: string): string {
  const td = new TurndownService({ headingStyle: "atx", codeBlockStyle: "fenced" });
  td.addRule("noteAssetImage", {
    filter(node) {
      return node.nodeName === "IMG" && Boolean((node as HTMLImageElement).getAttribute("data-note-asset"));
    },
    replacement(_content, node) {
      const el = node as HTMLImageElement;
      const alt = el.getAttribute("alt") ?? "";
      const asset = el.getAttribute("data-note-asset") ?? el.getAttribute("src") ?? "";
      return `![${alt}](${asset})`;
    },
  });
  return td.turndown(html);
}

export async function resolveAssetPathsInMarkdown(
  markdown: string,
  resolve: (assetPath: string) => Promise<string | null>,
): Promise<string> {
  const matches = [...markdown.matchAll(ASSET_MD_RE)];
  if (matches.length === 0) return markdown;
  let out = markdown;
  for (const m of matches) {
    const full = m[0];
    const alt = m[1] ?? "";
    const assetPath = m[2] ?? "";
    const dataUrl = await resolve(assetPath);
    if (dataUrl) {
      out = out.replace(full, `![${alt}](${dataUrl})`);
    }
  }
  return out;
}

export function injectNoteAssetAttributes(html: string, noteId: string): string {
  if (!noteId || typeof DOMParser === "undefined") return html;
  try {
    const doc = new DOMParser().parseFromString(html, "text/html");
    for (const img of doc.querySelectorAll("img")) {
      const src = img.getAttribute("src") ?? "";
      if (src.startsWith("data:")) continue;
      const assetMatch = src.match(/\/api\/notes\/[^/]+\/assets\/([^/?#]+)/);
      if (assetMatch?.[1]) {
        img.setAttribute("data-note-asset", `assets/${decodeURIComponent(assetMatch[1])}`);
      }
    }
    return doc.body.innerHTML;
  } catch {
    return html;
  }
}
