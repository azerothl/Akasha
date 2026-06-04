import { useEffect, useMemo, useRef, useState } from "react";
import { clearResearchPreviewHtml, parseResearchPreviewHash, readResearchPreviewHtml } from "./researchReportExport";

export function ResearchPreviewPage() {
  const { key, autoPrint } = useMemo(() => parseResearchPreviewHash(window.location.hash), []);
  const [html, setHtml] = useState<string | null>(null);
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const loadedRef = useRef(false);

  useEffect(() => {
    if (!key || loadedRef.current) return;
    const stored = readResearchPreviewHtml(key);
    if (!stored) return;
    loadedRef.current = true;
    setHtml(stored);
  }, [key]);

  useEffect(() => {
    if (!autoPrint || !html) return;
    const iframe = iframeRef.current;
    if (!iframe) return;
    const triggerPrint = () => {
      try {
        iframe.contentWindow?.focus();
        iframe.contentWindow?.print();
      } catch {
        /* user may cancel */
      }
    };
    const onLoad = () => window.setTimeout(triggerPrint, 400);
    iframe.addEventListener("load", onLoad);
    if (iframe.contentDocument?.readyState === "complete") onLoad();
    return () => iframe.removeEventListener("load", onLoad);
  }, [autoPrint, html]);

  const onIframeLoad = () => {
    if (key) clearResearchPreviewHtml(key);
  };

  if (!key) {
    return (
      <main className="research-preview-empty">
        <p>Lien de prévisualisation invalide.</p>
      </main>
    );
  }

  if (!html) {
    return (
      <main className="research-preview-empty">
        <p>Rapport introuvable ou déjà ouvert.</p>
      </main>
    );
  }

  return (
    <iframe
      ref={iframeRef}
      title="Research report preview"
      className="research-preview-frame"
      srcDoc={html}
      sandbox="allow-same-origin allow-scripts allow-modals"
      onLoad={onIframeLoad}
    />
  );
}
