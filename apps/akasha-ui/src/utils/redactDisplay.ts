/** Patterns for secrets that should be blurred in assistant chat output. */
const SECRET_PATTERNS: RegExp[] = [
  // OpenAI / generic API keys
  /\bsk-[A-Za-z0-9_-]{20,}\b/g,
  // Anthropic
  /\bsk-ant-[A-Za-z0-9_-]{20,}\b/g,
  // GitHub PAT / fine-grained tokens
  /\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}\b/g,
  // Bearer tokens in headers or inline
  /\bBearer\s+[A-Za-z0-9._~+/=-]{20,}\b/gi,
  // AWS access key id
  /\bAKIA[0-9A-Z]{16}\b/g,
  // Slack tokens
  /\bxox[baprs]-[A-Za-z0-9-]{10,}\b/g,
  // Generic long hex / base64 secrets after key= or token=
  /(?:api[_-]?key|token|secret|password|passwd|authorization)\s*[:=]\s*['"]?([A-Za-z0-9+/=_-]{16,})['"]?/gi,
  // JWT (three base64url segments)
  /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b/g,
];

const REDACTED = "••••••••";

/**
 * Redact likely API keys, tokens, and secrets from display text.
 * Applied to assistant messages before rendering.
 */
export function redactDisplaySecrets(text: string): string {
  if (!text) return text;
  let out = text;
  for (const pattern of SECRET_PATTERNS) {
    out = out.replace(pattern, (match, ...groups) => {
      if (typeof groups[0] === "string" && groups[0].length >= 16) {
        const secret = groups[0];
        const start = match.indexOf(secret);
        const end = start + secret.length;
        return `${match.slice(0, start)}${REDACTED}${match.slice(end)}`;
      }
      return REDACTED;
    });
  }
  return out;
}
