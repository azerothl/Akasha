import { Component, Suspense, lazy, createElement, forwardRef, type ComponentType, type ComponentProps, type ComponentRef, type ReactNode } from "react";
import { useI18n } from "../useI18n";
import { PRESERVE_DRAFT_EVENT } from "../hooks/useChatDraftRecovery";

class PanelErrorBoundary extends Component<{ children: ReactNode; en: boolean }, { failed: boolean; reloadBlocked: boolean }> {
  state = { failed: false, reloadBlocked: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() {
    if (!this.state.failed) return this.props.children;
    return <div className="panel-load-state" role="alert">
      <p>{this.props.en ? "This panel could not be loaded. Your work in the other panels is preserved." : "Ce panneau n’a pas pu être chargé. Votre travail dans les autres panneaux est conservé."}</p>
      <p>{this.props.en ? "Your chat draft will be restored after reloading. Let any pending note save finish first." : "Votre brouillon de chat sera restauré après le rechargement. Laissez d’abord se terminer l’enregistrement de vos notes."}</p>
      {this.state.reloadBlocked && <p>{this.props.en ? "The draft could not be saved. Go back to Chat and copy it before restarting." : "Le brouillon n’a pas pu être conservé. Revenez au Chat et copiez-le avant de redémarrer."}</p>}
      <button type="button" className="btn-secondary" onClick={() => { window.location.hash = "#/chat"; }}>{this.props.en ? "Back to Chat" : "Retour au Chat"}</button>{" "}
      <button type="button" className="btn-primary" onClick={() => {
        const preserved = window.dispatchEvent(new Event(PRESERVE_DRAFT_EVENT, { cancelable: true }));
        if (preserved) window.location.reload();
        else this.setState({ reloadBlocked: true });
      }}>{this.props.en ? "Reload interface" : "Recharger l’interface"}</button>
    </div>;
  }
}

/** A separate boundary per panel keeps the composer usable while a chunk loads or fails. */
// Match React.lazy's component constraint; the exported wrapper preserves the exact props.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function deferPanel<T extends ComponentType<any>>(load: () => Promise<{ default: T }>) {
  const Panel = lazy(load);
  return forwardRef<ComponentRef<T>, ComponentProps<T>>(function DeferredPanel(props, ref) {
    const { locale } = useI18n();
    return <PanelErrorBoundary en={locale === "en"}>
      <Suspense fallback={<p className="panel-load-state" role="status">{locale === "en" ? "Loading panel…" : "Chargement du panneau…"}</p>}>
        {createElement(Panel, { ...props, ref } as ComponentProps<T>)}
      </Suspense>
    </PanelErrorBoundary>;
  });
}
