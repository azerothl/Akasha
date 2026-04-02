/** Escape for HTML attribute value (e.g. alt). */
function escapeHtmlAttr(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/** Validates that a data URL is a safe image data URL (data:image/<type>;base64,<payload>). */
function isValidDataImageUrl(url: string): boolean {
  return /^data:image\/[a-zA-Z0-9+\-]+(?:;[a-zA-Z0-9\-=]+)*;base64,[A-Za-z0-9+/]+=*$/.test(url);
}

/** Validates that a data URL is a safe audio data URL (data:audio/<type>;base64,<payload>). */
function isValidDataAudioUrl(url: string): boolean {
  return /^data:audio\/[a-zA-Z0-9+\-]+(?:;[a-zA-Z0-9\-=]+)*;base64,[A-Za-z0-9+/]+=*$/.test(url);
}

/**
 * Convert markdown image syntax with data: URLs to raw <img> so they bypass
 * micromark's URI sanitizer (which strips data: and leaves src empty).
 * Run before path preprocessing so base64 is not corrupted.
 * Uses a two-step approach to avoid regex limits on very long base64 strings.
 */
export function preprocessDataUrlImages(text: string): string {
  if (!text || typeof text !== "string") return text;
  let result = text;

  /** Reference-style with angle brackets: ![alt](<data:image/...>) */
  const markerAngle = "](<data:image/";
  let idx: number;
  while ((idx = result.indexOf(markerAngle)) !== -1) {
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
    if (!isValidDataImageUrl(url)) {
      result = result.slice(0, idx) + result.slice(idx + markerAngle.length);
      continue;
    }
    const img = `\n\n<div class="markdown-data-image-wrap"><img src="${escapeHtmlAttr(url)}" alt="${escapeHtmlAttr(alt)}" class="markdown-data-image" /></div>\n\n`;
    result = result.slice(0, fullMatchStart) + img + result.slice(fullMatchEnd);
  }

  /** Common markdown without angle brackets: ![alt](data:image/...;base64,...)
   *  micromark strips data: in link destinations, so we must inject raw HTML here too. */
  const markerPlain = "](data:image/";
  while ((idx = result.indexOf(markerPlain)) !== -1) {
    const altStart = result.lastIndexOf("![", idx);
    const alt = altStart !== -1 ? result.slice(altStart + 2, idx) : "Image";
    const urlStart = idx + 2; // start of "data:image/"
    const closeParen = result.indexOf(")", urlStart);
    const url = closeParen !== -1 ? result.slice(urlStart, closeParen) : result.slice(urlStart);
    const fullMatchStart = altStart !== -1 ? altStart : idx;
    const fullMatchEnd = closeParen !== -1 ? closeParen + 1 : result.length;
    const validPlain = isValidDataImageUrl(url);
    if (!validPlain) {
      result = result.slice(0, idx) + result.slice(idx + markerPlain.length);
      continue;
    }
    const img = `\n\n<div class="markdown-data-image-wrap"><img src="${escapeHtmlAttr(url)}" alt="${escapeHtmlAttr(alt)}" class="markdown-data-image" /></div>\n\n`;
    result = result.slice(0, fullMatchStart) + img + result.slice(fullMatchEnd);
  }

  return result;
}

const AUDIO_MARKER = "](<data:audio/";

/**
 * Convert markdown image syntax with data:audio/ URLs to <audio controls> for TTS playback.
 * Same pattern as preprocessDataUrlImages but for audio (Kyutai TTS, speech_synthesize tool).
 */
export function preprocessDataUrlAudio(text: string): string {
  if (!text || typeof text !== "string") return text;
  let result = text;
  let idx: number;
  while ((idx = result.indexOf(AUDIO_MARKER)) !== -1) {
    const altStart = result.lastIndexOf("![", idx);
    const urlStart = idx + 3; // start of "data:audio/"
    const closeBracket = result.indexOf(">)", urlStart);
    const url =
      closeBracket !== -1
        ? result.slice(urlStart, closeBracket)
        : result.slice(urlStart);
    const fullMatchStart = altStart !== -1 ? altStart : idx;
    const fullMatchEnd = closeBracket !== -1 ? closeBracket + 2 : result.length;
    if (!isValidDataAudioUrl(url)) {
      result = result.slice(0, idx) + result.slice(idx + AUDIO_MARKER.length);
      continue;
    }
    const audio = `\n\n<div class="markdown-data-audio-wrap"><audio controls src="${escapeHtmlAttr(url)}" class="markdown-data-audio"></audio></div>\n\n`;
    result = result.slice(0, fullMatchStart) + audio + result.slice(fullMatchEnd);
  }
  return result;
}
