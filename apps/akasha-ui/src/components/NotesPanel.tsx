import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "akasha_notes_tabs_v1";

type NoteTab = {
  id: string;
  title: string;
  content: string;
};

function loadTabs(): NoteTab[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as NoteTab[];
      if (Array.isArray(parsed) && parsed.length > 0) {
        return parsed.map((t) => ({
          id: String(t.id ?? crypto.randomUUID()),
          title: String(t.title ?? "Note"),
          content: String(t.content ?? ""),
        }));
      }
    }
  } catch {
    /* ignore */
  }
  return [{ id: crypto.randomUUID(), title: "Note 1", content: "" }];
}

function saveTabs(tabs: NoteTab[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(tabs));
  } catch {
    /* ignore */
  }
}

type Props = {
  t: (key: string) => string;
};

export function NotesPanel({ t }: Props) {
  const [tabs, setTabs] = useState<NoteTab[]>(loadTabs);
  const [activeId, setActiveId] = useState<string>(() => tabs[0]?.id ?? "");

  useEffect(() => {
    saveTabs(tabs);
    if (!tabs.some((tab) => tab.id === activeId)) {
      setActiveId(tabs[0]?.id ?? "");
    }
  }, [tabs, activeId]);

  const active = tabs.find((tab) => tab.id === activeId) ?? tabs[0];

  const addTab = useCallback(() => {
    const id = crypto.randomUUID();
    setTabs((prev) => [...prev, { id, title: `${t("notes.default_title")} ${prev.length + 1}`, content: "" }]);
    setActiveId(id);
  }, [t]);

  const removeTab = useCallback(
    (id: string) => {
      setTabs((prev) => {
        if (prev.length <= 1) return prev;
        return prev.filter((tab) => tab.id !== id);
      });
    },
    [],
  );

  const updateActive = useCallback(
    (patch: Partial<Pick<NoteTab, "title" | "content">>) => {
      if (!active) return;
      setTabs((prev) => prev.map((tab) => (tab.id === active.id ? { ...tab, ...patch } : tab)));
    },
    [active],
  );

  return (
    <div className="notes-panel">
      <div className="notes-panel-header">
        <h2 className="panel-title">{t("tabs.notes")}</h2>
        <button type="button" className="btn-secondary notes-add-tab" onClick={addTab}>
          {t("notes.add_tab")}
        </button>
      </div>
      <div className="notes-tab-bar" role="tablist" aria-label={t("notes.tab_list")}>
        {tabs.map((tab) => (
          <div key={tab.id} className={`notes-tab-item ${tab.id === activeId ? "active" : ""}`}>
            <button
              type="button"
              role="tab"
              aria-selected={tab.id === activeId}
              className="notes-tab-btn"
              onClick={() => setActiveId(tab.id)}
            >
              {tab.title.trim() || t("notes.untitled")}
            </button>
            {tabs.length > 1 ? (
              <button
                type="button"
                className="notes-tab-close"
                aria-label={t("notes.close_tab")}
                onClick={() => removeTab(tab.id)}
              >
                ×
              </button>
            ) : null}
          </div>
        ))}
      </div>
      {active ? (
        <div className="notes-editor" role="tabpanel">
          <input
            type="text"
            className="settings-input notes-title-input"
            value={active.title}
            onChange={(e) => updateActive({ title: e.target.value.slice(0, 80) })}
            placeholder={t("notes.title_placeholder")}
            aria-label={t("notes.title_placeholder")}
          />
          <textarea
            className="settings-textarea notes-content"
            rows={18}
            value={active.content}
            onChange={(e) => updateActive({ content: e.target.value })}
            placeholder={t("notes.content_placeholder")}
            aria-label={t("notes.content_placeholder")}
          />
        </div>
      ) : null}
    </div>
  );
}
