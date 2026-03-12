import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useI18n } from "./useI18n";

export { preprocessMessagePaths } from "./preprocessMessagePaths";

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
  return (
    <div className={className ?? "markdown-rendered"}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
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
        {children}
      </ReactMarkdown>
    </div>
  );
}
