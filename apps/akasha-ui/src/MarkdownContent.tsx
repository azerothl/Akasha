import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** Lazy-loaded markdown renderer to reduce initial bundle (react-markdown + remark-gfm in separate chunk). */
export default function MarkdownContent({ children = "", className }: { children?: string; className?: string }) {
  return (
    <div className={className ?? "markdown-rendered"}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children: linkChildren, ...props }) => (
            <a href={href} target="_blank" rel="noopener noreferrer" {...props}>
              {linkChildren}
            </a>
          ),
        }}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
