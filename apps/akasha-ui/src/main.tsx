import React from "react";
import ReactDOM from "react-dom/client";
import { I18nProvider } from "./useI18n";
import { NotificationProvider } from "./notifications/NotificationContext";
import App from "./App";
import { ResearchPreviewPage } from "./ResearchPreviewPage";
import "./styles.css";
import {invoke} from "@tauri-apps/api/core";
import {initializeDaemonPort} from "./daemonPort";

function isResearchPreviewRoute(): boolean {
  const raw = window.location.hash.replace(/^#\/?/, "").split("?")[0]?.trim() ?? "";
  return raw === "research-preview";
}

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

async function start(){
await initializeDaemonPort("__TAURI_INTERNALS__" in window,()=>invoke("get_daemon_launch_config"));
if (isResearchPreviewRoute()) {
  root.render(
    <React.StrictMode>
      <ResearchPreviewPage />
    </React.StrictMode>,
  );
} else {
  root.render(
    <React.StrictMode>
      <I18nProvider>
        <NotificationProvider>
          <App />
        </NotificationProvider>
      </I18nProvider>
    </React.StrictMode>,
  );
}
}
void start().catch(()=>root.render(<p role="alert">Le lancement d’Akasha est incomplet. Fermez puis relancez l’application depuis son raccourci Windows.</p>));
