/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** When "true", browser build uses fetch() to the local daemon instead of Tauri invoke (Playwright E2E). */
  readonly VITE_E2E?: string;
}
