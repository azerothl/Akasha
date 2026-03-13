/** Escape for HTML attribute value (e.g. alt). */
function escapeHtmlAttr(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Convert markdown image syntax with data: URLs to raw <img> so they bypass
 * micromark's URI sanitizer (which strips data: and leaves src empty).
 * Run before path preprocessing so base64 is not corrupted.
 * Uses a two-step approach to avoid regex limits on very long base64 strings.
 */
export function preprocessDataUrlImages(text: string): string {
  if (!text || typeof text !== "string") return text;
  const marker = "](<data:image/";
  let result = text;
  let idx: number;
  while ((idx = result.indexOf(marker)) !== -1) {
    const altStart = result.lastIndexOf("![", idx);
    const alt = altStart !== -1 ? result.slice(altStart + 2, idx) : "Image";
    const urlStart = idx + 3; // start of "data:image/"
    const closeBracket = result.indexOf(">)", urlStart);
    const url =
      closeBracket !== -1
        ? result.slice(urlStart, closeBracket)
        : result.slice(urlStart);
    const fullMatchStart = altStart !== -1 ? altStart : idx;
    const fullMatchEnd = closeBracket !== -1 ? closeBracket + 2 : result.length;
    // Wrap in <div> so the markdown parser recognizes it as an HTML block (inline <img> alone may be escaped).
    const img = `\n\n<div class="markdown-data-image-wrap"><img src="${url}" alt="${escapeHtmlAttr(alt)}" class="markdown-data-image" /></div>\n\n`;
    result = result.slice(0, fullMatchStart) + img + result.slice(fullMatchEnd);
  }
  return result;
}
