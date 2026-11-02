import { invoke } from "@tauri-apps/api/core";
import { E2E_WEB } from "./e2eDaemon";

export type NoteMeta = {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
};

export type NoteDocument = NoteMeta & {
  content: string;
};

import { DAEMON_PORT as DEFAULT_PORT } from "./daemonPort";

function daemonBase(port?: number) {
  if (E2E_WEB) return "/__e2e_daemon";
  return `http://127.0.0.1:${port ?? DEFAULT_PORT}`;
}

async function fetchJson<T>(path: string, init?: RequestInit, port?: number): Promise<T> {
  const res = await fetch(`${daemonBase(port)}${path}`, init);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${text}`);
  }
  return res.json() as Promise<T>;
}

export async function listNotes(port?: number): Promise<NoteMeta[]> {
  try {
    const json = await invoke<{ notes?: NoteMeta[] }>("get_notes", { port: port ?? DEFAULT_PORT });
    return json.notes ?? [];
  } catch {
    const json = await fetchJson<{ notes?: NoteMeta[] }>("/api/notes", undefined, port);
    return json.notes ?? [];
  }
}

export async function getNote(id: string, port?: number): Promise<NoteDocument> {
  try {
    return await invoke<NoteDocument>("get_note", { id, port: port ?? DEFAULT_PORT });
  } catch {
    return fetchJson<NoteDocument>(`/api/notes/${encodeURIComponent(id)}`, undefined, port);
  }
}

export async function createNote(title: string, content = "", port?: number): Promise<NoteDocument> {
  try {
    return await invoke<NoteDocument>("create_note", { title, content, port: port ?? DEFAULT_PORT });
  } catch {
    return fetchJson<NoteDocument>(
      "/api/notes",
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ title, content }),
      },
      port,
    );
  }
}

export async function updateNote(
  id: string,
  patch: { title?: string; content?: string },
  port?: number,
): Promise<NoteDocument> {
  try {
    return await invoke<NoteDocument>("update_note", {
      id,
      title: patch.title ?? null,
      content: patch.content ?? null,
      port: port ?? DEFAULT_PORT,
    });
  } catch {
    return fetchJson<NoteDocument>(
      `/api/notes/${encodeURIComponent(id)}`,
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(patch),
      },
      port,
    );
  }
}

export async function deleteNoteApi(id: string, port?: number): Promise<void> {
  try {
    await invoke("delete_note", { id, port: port ?? DEFAULT_PORT });
  } catch {
    await fetchJson(`/api/notes/${encodeURIComponent(id)}`, { method: "DELETE" }, port);
  }
}

export async function uploadNoteAsset(
  noteId: string,
  file: File,
  port?: number,
): Promise<{ path: string; url: string }> {
  const content_base64 = await fileToBase64(file);
  const filename = file.name;
  const mime_type = file.type || "application/octet-stream";
  try {
    const json = await invoke<{ path: string; url: string }>("upload_note_asset", {
      id: noteId,
      filename,
      content_base64,
      mime_type,
      port: port ?? DEFAULT_PORT,
    });
    return json;
  } catch {
    return fetchJson<{ path: string; url: string }>(
      `/api/notes/${encodeURIComponent(noteId)}/assets`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ filename, content_base64, mime_type }),
      },
      port,
    );
  }
}

export async function fetchNoteAssetDataUrl(
  noteId: string,
  assetPath: string,
  port?: number,
): Promise<string | null> {
  const filename = assetPath.replace(/^assets\//, "");
  if (!filename) return null;
  try {
    const json = await fetchJson<{ content_base64: string; mime_type: string }>(
      `/api/notes/${encodeURIComponent(noteId)}/assets/${encodeURIComponent(filename)}`,
      undefined,
      port,
    );
    if (!json.content_base64) return null;
    return `data:${json.mime_type || "application/octet-stream"};base64,${json.content_base64}`;
  } catch {
    return null;
  }
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result ?? "");
      const idx = result.indexOf(",");
      resolve(idx >= 0 ? result.slice(idx + 1) : result);
    };
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}
