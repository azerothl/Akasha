import { useCallback, useEffect, useState } from "react";
import { hashForTab, tabFromHash, type Tab } from "../navigation/types";

export function useHashRoute(defaultTab: Tab = "chat") {
  const read = useCallback((): Tab => tabFromHash(window.location.hash) ?? defaultTab, [defaultTab]);

  const [tab, setTabState] = useState<Tab>(read);

  const setTab = useCallback(
    (next: Tab) => {
      setTabState(next);
      const nextHash = hashForTab(next);
      if (window.location.hash !== nextHash) {
        window.history.replaceState(null, "", nextHash);
      }
    },
    [],
  );

  useEffect(() => {
    const onHash = () => setTabState(read());
    window.addEventListener("hashchange", onHash);
    if (!window.location.hash) {
      window.history.replaceState(null, "", hashForTab(defaultTab));
    } else {
      setTabState(read());
    }
    return () => window.removeEventListener("hashchange", onHash);
  }, [defaultTab, read]);

  return { tab, setTab };
}
