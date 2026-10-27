import { useEffect, useMemo, useRef, useState, type SyntheticEvent } from "react";
import { clearResearchPreviewHtml, parseResearchPreviewHash, readResearchPreviewHtml } from "./researchReportExport";

export function ResearchPreviewPage() {
  const { key, autoPrint } = useMemo(() => parseResearchPreviewHash(window.location.hash), []);
  const [html, setHtml] = useState<string | null>(null);
  const loadedRef = useRef(false);
  const autoPrintRequestedRef = useRef(false);
  const printTimeoutRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (!key || loadedRef.current) return;
    const stored = readResearchPreviewHtml(key);
    if (!stored) return;
    loadedRef.current = true;
    setHtml(stored);
  }, [key]);

  useEffect(() => {
    return () => {
      if (printTimeoutRef.current !== undefined) {
        window.clearTimeout(printTimeoutRef.current);
        printTimeoutRef.current = undefined;
      }
    };
  }, []);

  const onIframeLoad = (event: SyntheticEvent<HTMLIFrameElement>) => {
    if (key) clearResearchPreviewHtml(key);
    const iframe = event.currentTarget;
    const triggerPrint = () => {
      if (printTimeoutRef.current !== undefined) {
        window.clearTimeout(printTimeoutRef.current);
      }
      printTimeoutRef.current = undefined;
      try {
        iframe.contentWindow?.focus();
        iframe.contentWindow?.print();
      } catch {
        /* user may cancel */
      }
    };
    // The native preview CSP blocks inline onclick attributes.
    const printButton = iframe.contentDocument?.querySelector<HTMLButtonElement>(".research-report-print-bar button");
    if (printButton) printButton.onclick = triggerPrint;

    if (!autoPrint || autoPrintRequestedRef.current) return;
    // Consume the request before print() blocks, including any later load events.
    autoPrintRequestedRef.current = true;
    printTimeoutRef.current = window.setTimeout(triggerPrint, 400);
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
      title="Research report preview"
      className="research-preview-frame"
      srcDoc={html}
      sandbox="allow-same-origin allow-scripts allow-modals"
      onLoad={onIframeLoad}
    />
  );
}
