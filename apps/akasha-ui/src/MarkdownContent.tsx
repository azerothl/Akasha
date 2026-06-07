import { useState, useEffect } from "react";
import { createPortal } from "react-dom";
import { renderToStaticMarkup } from "react-dom/server";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import { useI18n } from "./useI18n";
import { preprocessDataUrlImages, preprocessDataUrlAudio } from "./preprocessDataUrlImages";

const SAFE_DATA_IMAGE_RE = /^data:image\/[a-zA-Z0-9+\-]+(?:;[a-zA-Z0-9\-=]+)*;base64,[A-Za-z0-9+/]+={0,2}$/;
const SAFE_DATA_AUDIO_RE = /^data:audio\/[a-zA-Z0-9+\-]+(?:;[a-zA-Z0-9\-=]+)*;base64,[A-Za-z0-9+/]+={0,2}$/;

/** react-markdown's defaultUrlTransform only allows http(s)/irc/mailto/xmpp; it strips `data:` to "" so <img src="data:..."> never reaches our component. */
function markdownUrlTransform(value: string, key: string) {
  const v = String(value ?? "");
  if (key === "src") {
    if (v.startsWith("data:image/") && SAFE_DATA_IMAGE_RE.test(v)) return v;
    if (v.startsWith("data:audio/") && SAFE_DATA_AUDIO_RE.test(v)) return v;
  }
  return defaultUrlTransform(v);
}

/** Sanitization schema: extends the safe default to allow the div/img/audio elements
 *  injected by preprocessDataUrlImages and preprocessDataUrlAudio, while blocking scripts.
 *  data: is allowed for src so that inline image and audio data URLs (from generate_image,
 *  speech_synthesize, camera) render. Event handlers and <script> remain blocked. */
const sanitizeSchema = {
  ...defaultSchema,
  tagNames: [...(defaultSchema.tagNames ?? []), "div", "audio", "img"],
  attributes: {
    ...defaultSchema.attributes,
    div: ["className", "class"],
    img: ["src", "alt", "className", "class", "title", "width", "height"],
    audio: ["src", "controls", "className", "class"],
  },
  protocols: {
    ...defaultSchema.protocols,
    src: [...(defaultSchema.protocols?.src ?? ["http", "https"]), "data"],
  },
};

export { preprocessMessagePaths } from "./preprocessMessagePaths";
export { preprocessDataUrlImages } from "./preprocessDataUrlImages";

const IMAGE_EXT = /\.(png|jpe?g|gif|webp)$/i;

function autoFormatLongUnstructuredPlainText(input: string): string {
  const text = input ?? "";
  const trimmed = text.trim();
  if (trimmed.length < 1200) return text;
  // If the content already has line breaks, don't guess formatting.
  if (text.includes("\n") || text.includes("\r")) return text;
  // Avoid touching obvious Markdown/code/data-url content.
  if (trimmed.includes("```") || trimmed.includes("##") || trimmed.includes("TOOL:") || trimmed.includes("data:") || trimmed.includes("http")) return text;

  let candidate = trimmed;

  // 1) Detect and expand ordered lists like: "1) ... 2) ..." into Markdown list:
  // Note: we only do this when we see multiple list-like markers to reduce false positives.
  const orderedMarkerCount = Array.from(candidate.matchAll(/\b\d+[\.\)]\s+/g)).length;
  if (orderedMarkerCount >= 2) {
    // Convert "1) " to "1. "
    candidate = candidate.replace(/\b(\d+)\)\s+/g, "$1. ");
    // Insert line breaks before " 2. " occurrences.
    candidate = candidate.replace(/\s(\d+)\.\s+/g, "\n\n$1. ");
  }

  // 2) Detect and expand bullet lists like: "- item1 • item2 * item3"
  const bulletMarkerCount = Array.from(candidate.matchAll(/(?:^|\s)[\-•\*]\s+/g)).length;
  if (bulletMarkerCount >= 2) {
    candidate = candidate.replace(/(?:^|\s)[\-•\*]\s+/g, "\n- ");
  }

  // 3) Detect section headers like "Résumé: ..." or "Conclusion: ..." and turn them into "## Header"
  const headingRe = /\b(?:Résumé|Summary|Conclusion|Plan|Étapes|Approche|Proposition|Étape)\b\s*:\s*/i;
  if (headingRe.test(candidate)) {
    candidate = candidate.replace(
      headingRe,
      (m) => {
        // Extract matched header word (first capturing group isn't available here, so re-run simpler)
        const wordMatch = m.match(/\b(?:Résumé|Summary|Conclusion|Plan|Étapes|Approche|Proposition|Étape)\b/i);
        const word = wordMatch?.[0] ?? "Section";
        return `\n\n## ${word}\n`;
      }
    );
  }

  // If we already created meaningful structure with newlines, keep it.
  const nonEmptyLines = candidate.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  if (nonEmptyLines.length >= 4) return candidate;

  // 4) Fallback: split into sentences and group into short paragraphs.
  // Heuristic: avoid unbroken paragraphs; keep only when we detect multiple sentence boundaries.
  const parts = candidate.split(/(?<=[.!?])\s+/);
  if (parts.length < 4) return text;

  const paragraphs: string[] = [];
  for (let i = 0; i < parts.length; i += 2) {
    const chunk = parts.slice(i, i + 2).join(" ").trim();
    if (chunk) paragraphs.push(chunk);
  }
  if (paragraphs.length < 2) return text;
  return paragraphs.join("\n\n");
}

function ImageThumbnail({ path }: { path: string }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    invoke<string>("read_file_as_data_url", { path })
      .then(setSrc)
      .catch(() => setSrc(null));
  }, [path]);
  if (!src) return null;
  return <img src={src} alt="" className="path-image-thumbnail" />;
}

function dataUrlDownloadFilename(src: string): string {
  const m = src.match(/^data:image\/(png|jpe?g|gif|webp)/i);
  if (!m) return "image.png";
  const ext = m[1].toLowerCase() === "jpeg" ? "jpg" : m[1];
  return `image.${ext}`;
}

/** Inline data:image/* from chat (e.g. generate_image): fit width, lightbox + download. */
function MarkdownDataImage({ src, alt }: { src: string; alt: string }) {
  const { t } = useI18n();
  const [lightbox, setLightbox] = useState(false);

  useEffect(() => {
    if (!lightbox) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setLightbox(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [lightbox]);

  useEffect(() => {
    if (!lightbox) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [lightbox]);

  const download = () => {
    const a = document.createElement("a");
    a.href = src;
    a.download = dataUrlDownloadFilename(src);
    a.rel = "noopener";
    a.click();
  };

  const lightboxUi =
    lightbox &&
    createPortal(
      <div
        className="markdown-image-lightbox"
        role="dialog"
        aria-modal="true"
        aria-label={t("markdown_image.view_full")}
        onClick={() => setLightbox(false)}
      >
        <button
          type="button"
          className="markdown-image-lightbox-close"
          aria-label={t("markdown_image.close")}
          onClick={(e) => {
            e.stopPropagation();
            setLightbox(false);
          }}
        >
          ×
        </button>
        <img src={src} alt={alt} className="markdown-image-lightbox-img" onClick={(e) => e.stopPropagation()} />
      </div>,
      document.body
    );

  return (
    <>
      <div className="markdown-data-image-wrap">
        <img src={src} alt={alt} className="markdown-data-image" loading="lazy" decoding="async" />
        <div className="markdown-data-image-toolbar">
          <button type="button" className="markdown-data-image-btn" onClick={() => setLightbox(true)}>
            {t("markdown_image.view_full")}
          </button>
          <button type="button" className="markdown-data-image-btn" onClick={download}>
            {t("markdown_image.download")}
          </button>
        </div>
      </div>
      {lightboxUi}
    </>
  );
}

type MarkdownContentProps = { children?: string; className?: string; onPathClick?: (path: string, openFolder?: boolean) => void };

/** Server-side HTML for research export / preview windows (same pipeline as chat markdown). */
export function renderMarkdownStaticHtml(children: string): string {
  const processed = preprocessDataUrlAudio(preprocessDataUrlImages(children));
  const pretty = autoFormatLongUnstructuredPlainText(processed);
  return renderToStaticMarkup(
    <div className="markdown-rendered">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw, [rehypeSanitize, sanitizeSchema]]}
        urlTransform={markdownUrlTransform}
        components={{
          img: ({ src, alt }) => {
            if (!src?.trim()) return null;
            if (src.startsWith("data:") && !SAFE_DATA_IMAGE_RE.test(src)) return null;
            return <img src={src} alt={alt ?? ""} />;
          },
          a: ({ href, children: linkChildren }) => (
            <a href={href} target="_blank" rel="noopener noreferrer">
              {linkChildren}
            </a>
          ),
        }}
      >
        {pretty}
      </ReactMarkdown>
    </div>
  );
}

/** Lazy-loaded markdown renderer to reduce initial bundle (react-markdown + remark-gfm in separate chunk). */
export default function MarkdownContent({ children = "", className, onPathClick }: MarkdownContentProps) {
  const { t } = useI18n();
  const processed = preprocessDataUrlAudio(preprocessDataUrlImages(children));
  const pretty = autoFormatLongUnstructuredPlainText(processed);
  return (
    <div className={className ?? "markdown-rendered"}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw, [rehypeSanitize, sanitizeSchema]]}
        urlTransform={markdownUrlTransform}
        components={{
          img: ({ src, alt, ...props }) => {
            if (!src || src.trim() === "") return null;
            // Reject data: URLs that don't match a safe image pattern to prevent XSS.
            if (src.startsWith("data:") && !SAFE_DATA_IMAGE_RE.test(src)) {
              return null;
            }
            if (src.startsWith("data:image/")) {
              return <MarkdownDataImage src={src} alt={alt ?? ""} />;
            }
            return <img src={src} alt={alt ?? ""} className="markdown-data-image markdown-data-image-remote" {...props} />;
          },
          audio: ({ src, ...props }) => {
            if (!src || src.trim() === "") return null;
            // Only allow safe data:audio/* or https sources.
            if (src.startsWith("data:") && !SAFE_DATA_AUDIO_RE.test(src)) return null;
            if (!src.startsWith("data:") && !src.startsWith("https://")) return null;
            return <audio src={src} controls className="markdown-data-audio" {...props} />;
          },
          a: ({ href, children: linkChildren, ...props }) => {
            if (href?.startsWith("path:") && onPathClick) {
              let path: string;
              try {
                path = decodeURIComponent(href.slice(5));
              } catch {
                path = href.slice(5);
              }
              return (
                <a
                  href="#"
                  role="button"
                  onClick={(e) => {
                    e.preventDefault();
                    onPathClick(path, false);
                  }}
                  className="path-link"
                  {...props}
                >
                  {linkChildren}
                </a>
              );
            }
            if (href?.startsWith("pathfolder:") && onPathClick) {
              let path: string;
              try {
                path = decodeURIComponent(href.slice(11));
              } catch {
                path = href.slice(11);
              }
              const isImage = IMAGE_EXT.test(path);
              return (
                <span className="path-link-block path-link-folder-block">
                  {isImage && <ImageThumbnail path={path} />}
                  <a
                    href="#"
                    role="button"
                    onClick={(e) => {
                      e.preventDefault();
                      onPathClick(path, true);
                    }}
                    className="path-link path-link-folder"
                    {...props}
                  >
                    {linkChildren}
                  </a>
                  {isImage && (
                    <>
                      <span className="path-link-folder-hint"> — </span>
                      <button
                        type="button"
                        className="path-link path-link-open-folder"
                        onClick={(e) => {
                          e.preventDefault();
                          onPathClick(path, true);
                        }}
                      >
                        {t("path_link.open_folder")}
                      </button>
                    </>
                  )}
                </span>
              );
            }
            return (
              <a href={href} target="_blank" rel="noopener noreferrer" {...props}>
                {linkChildren}
              </a>
            );
          },
        }}
      >
        {pretty}
      </ReactMarkdown>
    </div>
  );
}
