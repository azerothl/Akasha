import { useCallback, useEffect, useRef, useState } from "react";
import type { ChatNoteContext, ChatNoteIntent } from "../chatNoteContext";
import {
  createNote,
  deleteNoteApi,
  getNote,
  listNotes,
  updateNote,
  type NoteDocument,
  type NoteMeta,
} from "../notesApi";
import { NoteEditor } from "./NoteEditor";

const LEGACY_STORAGE_KEY = "akasha_notes_tabs_v1";
const MIGRATION_FLAG_KEY = "akasha_notes_migrated_v1";

type LegacyTab = { id: string; title: string; content: string };

function loadLegacyTabs(): LegacyTab[] {
  try {
    const raw = localStorage.getItem(LEGACY_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as LegacyTab[];
    if (!Array.isArray(parsed)) return [];
    return parsed.map((t) => ({
      id: String(t.id ?? crypto.randomUUID()),
      title: String(t.title ?? "Note"),
      content: String(t.content ?? ""),
    }));
  } catch {
    return [];
  }
}

type Props = {
  t: (key: string) => string;
  locale: "fr" | "en";
  daemonPort?: number;
  onDiscussNote?: (ctx: ChatNoteContext) => void;
};

export function NotesPanel({ t, daemonPort, onDiscussNote }: Props) {
  const [notes, setNotes] = useState<NoteMeta[]>([]);
  const [activeId, setActiveId] = useState<string>("");
  const [activeDoc, setActiveDoc] = useState<NoteDocument | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const pendingContent = useRef<string | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const titleTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshList = useCallback(async () => {
    const list = await listNotes(daemonPort);
    setNotes(list);
    return list;
  }, [daemonPort]);

  const loadNote = useCallback(
    async (id: string) => {
      const doc = await getNote(id, daemonPort);
      setActiveDoc(doc);
      pendingContent.current = doc.content;
      return doc;
    },
    [daemonPort],
  );

  const migrateLegacyIfNeeded = useCallback(
    async (existing: NoteMeta[]) => {
      if (existing.length > 0) return existing;
      if (localStorage.getItem(MIGRATION_FLAG_KEY) === "1") return existing;
      const legacy = loadLegacyTabs();
      if (legacy.length === 0) {
        localStorage.setItem(MIGRATION_FLAG_KEY, "1");
        return existing;
      }
      for (const tab of legacy) {
        await createNote(tab.title.trim() || t("notes.default_title"), tab.content, daemonPort);
      }
      localStorage.setItem(MIGRATION_FLAG_KEY, "1");
      localStorage.removeItem(LEGACY_STORAGE_KEY);
      return refreshList();
    },
    [daemonPort, refreshList, t],
  );

  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        let list = await refreshList();
        list = await migrateLegacyIfNeeded(list);
        if (cancelled) return;
        if (list.length === 0) {
          const created = await createNote(`${t("notes.default_title")} 1`, "", daemonPort);
          list = [created];
          setNotes(list);
          setActiveId(created.id);
          setActiveDoc(created);
        } else {
          setNotes(list);
          const firstId = list[0]?.id ?? "";
          setActiveId(firstId);
          if (firstId) await loadNote(firstId);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [daemonPort, refreshList, migrateLegacyIfNeeded, loadNote, t]);

  const selectNote = useCallback(
    async (id: string) => {
      if (id === activeId) return;
      setActiveId(id);
      setSaveState("idle");
      try {
        await loadNote(id);
      } catch (e) {
        setError(String(e));
      }
    },
    [activeId, loadNote],
  );

  const flushSave = useCallback(
    async (id: string, title?: string, content?: string) => {
      setSaveState("saving");
      try {
        const doc = await updateNote(
          id,
          {
            ...(title !== undefined ? { title } : {}),
            ...(content !== undefined ? { content } : {}),
          },
          daemonPort,
        );
        setActiveDoc(doc);
        setNotes((prev) =>
          prev.map((n) => (n.id === doc.id ? { ...n, title: doc.title, updated_at: doc.updated_at } : n)),
        );
        setSaveState("saved");
      } catch (e) {
        setSaveState("error");
        setError(String(e));
      }
    },
    [daemonPort],
  );

  const scheduleContentSave = useCallback(
    (content: string) => {
      if (!activeId) return;
      pendingContent.current = content;
      if (saveTimer.current) clearTimeout(saveTimer.current);
      saveTimer.current = setTimeout(() => {
        void flushSave(activeId, undefined, content);
      }, 800);
    },
    [activeId, flushSave],
  );

  const updateTitle = useCallback(
    (title: string) => {
      if (!activeDoc) return;
      const trimmed = title.slice(0, 80);
      setActiveDoc({ ...activeDoc, title: trimmed });
      setNotes((prev) => prev.map((n) => (n.id === activeDoc.id ? { ...n, title: trimmed } : n)));
      if (titleTimer.current) clearTimeout(titleTimer.current);
      titleTimer.current = setTimeout(() => {
        void flushSave(activeDoc.id, trimmed, pendingContent.current ?? activeDoc.content);
      }, 400);
    },
    [activeDoc, flushSave],
  );

  const addTab = useCallback(async () => {
    try {
      const doc = await createNote(`${t("notes.default_title")} ${notes.length + 1}`, "", daemonPort);
      setNotes((prev) => [doc, ...prev]);
      setActiveId(doc.id);
      setActiveDoc(doc);
      pendingContent.current = "";
    } catch (e) {
      setError(String(e));
    }
  }, [notes.length, daemonPort, t]);

  const removeTab = useCallback(
    async (id: string) => {
      if (notes.length <= 1) return;
      try {
        await deleteNoteApi(id, daemonPort);
        const next = notes.filter((n) => n.id !== id);
        setNotes(next);
        if (activeId === id) {
          const fallback = next[0]?.id ?? "";
          setActiveId(fallback);
          if (fallback) await loadNote(fallback);
          else setActiveDoc(null);
        }
      } catch (e) {
        setError(String(e));
      }
    },
    [notes, activeId, daemonPort, loadNote],
  );

  const dispatchAgentAction = useCallback(
    (intent: ChatNoteIntent) => {
      if (!activeDoc || !onDiscussNote) return;
      const md = pendingContent.current ?? activeDoc.content;
      if (!md.trim() && intent === "discuss") return;
      onDiscussNote({
        noteId: activeDoc.id,
        title: activeDoc.title,
        markdown: md,
        intent,
      });
    },
    [activeDoc, onDiscussNote],
  );

  if (loading) {
    return (
      <div className="notes-panel">
        <p className="panel-loading">{t("common.loading")}</p>
      </div>
    );
  }

  return (
    <div className="notes-panel">
      <div className="notes-panel-header">
        <h2 className="panel-title">{t("tabs.notes")}</h2>
        <div className="notes-panel-header-actions">
          {saveState === "saving" ? (
            <span className="notes-save-status">{t("notes.saving")}</span>
          ) : saveState === "saved" ? (
            <span className="notes-save-status">{t("notes.saved")}</span>
          ) : null}
          <button type="button" className="btn-secondary notes-add-tab" onClick={() => void addTab()}>
            {t("notes.add_tab")}
          </button>
        </div>
      </div>
      {error ? (
        <p className="notes-error" role="alert">
          {error}
        </p>
      ) : null}
      <div className="notes-tab-bar" role="tablist" aria-label={t("notes.tab_list")}>
        {notes.map((tab) => (
          <div key={tab.id} className={`notes-tab-item ${tab.id === activeId ? "active" : ""}`}>
            <button
              type="button"
              role="tab"
              aria-selected={tab.id === activeId}
              className="notes-tab-btn"
              onClick={() => void selectNote(tab.id)}
            >
              {tab.title.trim() || t("notes.untitled")}
            </button>
            {notes.length > 1 ? (
              <button
                type="button"
                className="notes-tab-close"
                aria-label={t("notes.close_tab")}
                onClick={() => void removeTab(tab.id)}
              >
                ×
              </button>
            ) : null}
          </div>
        ))}
      </div>
      {activeDoc ? (
        <div className="notes-editor" role="tabpanel">
          <input
            type="text"
            className="settings-input notes-title-input"
            value={activeDoc.title}
            onChange={(e) => updateTitle(e.target.value)}
            placeholder={t("notes.title_placeholder")}
            aria-label={t("notes.title_placeholder")}
          />
          <div className="notes-agent-actions">
            <button type="button" className="btn-secondary" onClick={() => dispatchAgentAction("discuss")}>
              {t("notes.action_discuss")}
            </button>
            <button type="button" className="btn-secondary" onClick={() => dispatchAgentAction("improve")}>
              {t("notes.action_improve")}
            </button>
            <button type="button" className="btn-secondary" onClick={() => dispatchAgentAction("validate")}>
              {t("notes.action_validate")}
            </button>
            <button type="button" className="btn-secondary" onClick={() => dispatchAgentAction("add_references")}>
              {t("notes.action_references")}
            </button>
          </div>
          <NoteEditor
            key={activeDoc.id}
            noteId={activeDoc.id}
            markdown={activeDoc.content}
            placeholder={t("notes.content_placeholder")}
            daemonPort={daemonPort}
            t={t}
            onMarkdownChange={scheduleContentSave}
          />
        </div>
      ) : null}
    </div>
  );
}

export type { ChatNoteContext };
