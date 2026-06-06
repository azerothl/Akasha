import React from "react";
import ReactDOM from "react-dom/client";
import { I18nProvider } from "./useI18n";
import { NotificationProvider } from "./notifications/NotificationContext";
import App from "./App";
import { ResearchPreviewPage } from "./ResearchPreviewPage";
import "./styles.css";

function isResearchPreviewRoute(): boolean {
  const raw = window.location.hash.replace(/^#\/?/, "").split("?")[0]?.trim() ?? "";
  return raw === "research-preview";
}

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

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
