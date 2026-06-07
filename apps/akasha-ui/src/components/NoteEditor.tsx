import { useCallback, useEffect, useRef } from "react";
import { useEditor, EditorContent } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import Image from "@tiptap/extension-image";
import Link from "@tiptap/extension-link";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import Placeholder from "@tiptap/extension-placeholder";
import Underline from "@tiptap/extension-underline";
import {
  fetchNoteAssetDataUrl,
  uploadNoteAsset,
} from "../notesApi";
import {
  htmlToMarkdown,
  injectNoteAssetAttributes,
  markdownToHtml,
  resolveAssetPathsInMarkdown,
} from "../noteMarkdown";

const NoteImage = Image.extend({
  addAttributes() {
    return {
      ...this.parent?.(),
      dataNoteAsset: {
        default: null,
        parseHTML: (element) => element.getAttribute("data-note-asset"),
        renderHTML: (attributes) =>
          attributes.dataNoteAsset
            ? { "data-note-asset": attributes.dataNoteAsset as string }
            : {},
      },
    };
  },
});

type Props = {
  noteId: string;
  markdown: string;
  placeholder: string;
  daemonPort?: number;
  t: (key: string) => string;
  onMarkdownChange: (markdown: string) => void;
  disabled?: boolean;
};

export function NoteEditor({
  noteId,
  markdown,
  placeholder,
  daemonPort,
  t,
  onMarkdownChange,
  disabled,
}: Props) {
  const skipNextUpdate = useRef(true);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const editor = useEditor({
    extensions: [
      StarterKit.configure({ heading: { levels: [1, 2, 3] } }),
      Underline,
      Link.configure({ openOnClick: false, autolink: true }),
      NoteImage.configure({ inline: false, allowBase64: true }),
      TaskList,
      TaskItem.configure({ nested: true }),
      Placeholder.configure({ placeholder }),
    ],
    editable: !disabled,
    onUpdate: ({ editor: ed }) => {
      if (skipNextUpdate.current) return;
      const html = ed.getHTML();
      const md = htmlToMarkdown(html);
      onMarkdownChange(md);
    },
  });

  const loadMarkdownIntoEditor = useCallback(
    async (md: string) => {
      if (!editor) return;
      skipNextUpdate.current = true;
      const resolved = await resolveAssetPathsInMarkdown(md, (assetPath) =>
        fetchNoteAssetDataUrl(noteId, assetPath, daemonPort),
      );
      let html = markdownToHtml(resolved);
      html = injectNoteAssetAttributes(html, noteId);
      editor.commands.setContent(html, { emitUpdate: false });
      requestAnimationFrame(() => {
        skipNextUpdate.current = false;
      });
    },
    [editor, noteId, daemonPort],
  );

  useEffect(() => {
    if (!editor) return;
    void loadMarkdownIntoEditor(markdown);
  }, [editor, noteId]); // eslint-disable-line react-hooks/exhaustive-deps -- reload on note switch only

  useEffect(() => {
    if (!editor) return;
    editor.setEditable(!disabled);
  }, [editor, disabled]);

  const insertImageFile = useCallback(
    async (file: File) => {
      if (!editor || !file.type.startsWith("image/")) return;
      try {
        const { path } = await uploadNoteAsset(noteId, file, daemonPort);
        const dataUrl = await fetchNoteAssetDataUrl(noteId, path, daemonPort);
        editor
          .chain()
          .focus()
          .setImage({
            src: dataUrl ?? path,
            alt: file.name,
            dataNoteAsset: path,
          } as { src: string; alt: string; dataNoteAsset: string })
          .run();
        const md = htmlToMarkdown(editor.getHTML());
        onMarkdownChange(md);
      } catch {
        /* ignore upload errors */
      }
    },
    [editor, noteId, daemonPort, onMarkdownChange],
  );

  const onImagePick = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      e.target.value = "";
      if (file) void insertImageFile(file);
    },
    [insertImageFile],
  );

  useEffect(() => {
    if (!editor) return;
    const el = editor.view.dom;
    const onPaste = (ev: ClipboardEvent) => {
      const item = [...(ev.clipboardData?.items ?? [])].find((i) => i.type.startsWith("image/"));
      if (!item) return;
      const file = item.getAsFile();
      if (file) {
        ev.preventDefault();
        void insertImageFile(file);
      }
    };
    const onDrop = (ev: DragEvent) => {
      const file = [...(ev.dataTransfer?.files ?? [])].find((f) => f.type.startsWith("image/"));
      if (!file) return;
      ev.preventDefault();
      void insertImageFile(file);
    };
    el.addEventListener("paste", onPaste);
    el.addEventListener("drop", onDrop);
    return () => {
      el.removeEventListener("paste", onPaste);
      el.removeEventListener("drop", onDrop);
    };
  }, [editor, insertImageFile]);

  if (!editor) return null;

  const btn = (label: string, action: () => void, active?: boolean) => (
    <button
      type="button"
      className={`notes-toolbar-btn${active ? " active" : ""}`}
      onClick={action}
      aria-label={label}
      title={label}
    >
      {label}
    </button>
  );

  return (
    <div className="note-editor">
      <div className="notes-toolbar" role="toolbar" aria-label={t("notes.toolbar")}>
        {btn("B", () => editor.chain().focus().toggleBold().run(), editor.isActive("bold"))}
        {btn("I", () => editor.chain().focus().toggleItalic().run(), editor.isActive("italic"))}
        {btn("U", () => editor.chain().focus().toggleUnderline().run(), editor.isActive("underline"))}
        {btn("H1", () => editor.chain().focus().toggleHeading({ level: 1 }).run(), editor.isActive("heading", { level: 1 }))}
        {btn("H2", () => editor.chain().focus().toggleHeading({ level: 2 }).run(), editor.isActive("heading", { level: 2 }))}
        {btn("H3", () => editor.chain().focus().toggleHeading({ level: 3 }).run(), editor.isActive("heading", { level: 3 }))}
        {btn("•", () => editor.chain().focus().toggleBulletList().run(), editor.isActive("bulletList"))}
        {btn("1.", () => editor.chain().focus().toggleOrderedList().run(), editor.isActive("orderedList"))}
        {btn("☑", () => editor.chain().focus().toggleTaskList().run(), editor.isActive("taskList"))}
        {btn("</>", () => editor.chain().focus().toggleCodeBlock().run(), editor.isActive("codeBlock"))}
        {btn("🔗", () => {
          const url = window.prompt(t("notes.link_prompt"));
          if (url) editor.chain().focus().setLink({ href: url }).run();
        }, editor.isActive("link"))}
        {btn("🖼", () => fileInputRef.current?.click(), false)}
        <input
          ref={fileInputRef}
          type="file"
          accept="image/*"
          className="sr-only"
          aria-hidden
          onChange={onImagePick}
        />
      </div>
      <EditorContent editor={editor} className="notes-tiptap-content" />
    </div>
  );
}
