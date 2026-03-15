import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import { useI18n } from "./useI18n";
import { preprocessDataUrlImages } from "./preprocessDataUrlImages";

/** Sanitization schema: extends the safe default to allow the div/img elements
 *  injected by preprocessDataUrlImages, while blocking scripts and other dangerous tags.
 *  Security note: only the default safe protocols (e.g. http/https) are allowed for `src`;
 *  `data:` URLs are intentionally disallowed here to reduce the XSS surface area from
 *  SVG-in-image and similar edge cases. Event handlers (onerror, onclick, etc.) and
 *  <script> tags remain blocked by the schema. */
const sanitizeSchema = {
  ...defaultSchema,
  tagNames: [...(defaultSchema.tagNames ?? []), "div"],
  attributes: {
    ...defaultSchema.attributes,
    div: ["className", "class"],
    img: ["src", "alt", "className", "class", "title", "width", "height"],
  },
  protocols: {
    ...defaultSchema.protocols,
    src: [...(defaultSchema.protocols?.src ?? ["http", "https"])],
  },
};

export { preprocessMessagePaths } from "./preprocessMessagePaths";
export { preprocessDataUrlImages } from "./preprocessDataUrlImages";

const IMAGE_EXT = /\.(png|jpe?g|gif|webp)$/i;

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

type MarkdownContentProps = { children?: string; className?: string; onPathClick?: (path: string, openFolder?: boolean) => void };

/** Lazy-loaded markdown renderer to reduce initial bundle (react-markdown + remark-gfm in separate chunk). */
export default function MarkdownContent({ children = "", className, onPathClick }: MarkdownContentProps) {
  const { t } = useI18n();
  const processed = preprocessDataUrlImages(children);
  return (
    <div className={className ?? "markdown-rendered"}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw, [rehypeSanitize, sanitizeSchema]]}
        components={{
          img: ({ src, alt, ...props }) => {
            if (!src || src.trim() === "") return null;
            return <img src={src} alt={alt ?? ""} className="markdown-data-image" {...props} />;
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
        {processed}
      </ReactMarkdown>
    </div>
  );
}
