import React, { createContext, useCallback, useContext, useMemo, useState } from "react";
import fr from "./locales/fr.json";
import en from "./locales/en.json";

const LOCALE_STORAGE_KEY = "akasha_locale";
type LocaleId = "fr" | "en";

const messages: Record<LocaleId, Record<string, string>> = { fr, en };

function getInitialLocale(): LocaleId {
  try {
    const saved = localStorage.getItem(LOCALE_STORAGE_KEY) as LocaleId | null;
    if (saved === "en" || saved === "fr") return saved;
    const nav = typeof navigator !== "undefined" ? navigator.language : "";
    if (nav.startsWith("en")) return "en";
  } catch {
    /* ignore */
  }
  return "fr";
}

interface I18nContextValue {
  t: (key: string) => string;
  locale: LocaleId;
  setLocale: (id: LocaleId) => void;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [locale, setLocaleState] = useState<LocaleId>(getInitialLocale);

  const setLocale = useCallback((id: LocaleId) => {
    setLocaleState(id);
    try {
      localStorage.setItem(LOCALE_STORAGE_KEY, id);
    } catch {
      /* ignore */
    }
  }, []);

  const t = useCallback(
    (key: string): string => {
      const dict = messages[locale] ?? messages.fr;
      return (dict as Record<string, string>)[key] ?? key;
    },
    [locale]
  );

  const value = useMemo(() => ({ t, locale, setLocale }), [t, locale, setLocale]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (!ctx) throw new Error("useI18n must be used within I18nProvider");
  return ctx;
}
