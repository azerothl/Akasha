import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useFocusScope } from "./useFocusScope";

export function useMobileSidebar() {
  const [narrow, setNarrow] = useState(() => window.matchMedia("(max-width: 768px)").matches);
  const [open, setOpen] = useState(false);
  const sidebarRef = useRef<HTMLElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const media = window.matchMedia("(max-width: 768px)");
    const change = () => { setNarrow(media.matches); setOpen(false); };
    media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);
  useLayoutEffect(() => {
    const sidebar = sidebarRef.current, content = contentRef.current;
    if (!sidebar || !content) return;
    const hidden = narrow && !open;
    if (hidden && sidebar.contains(document.activeElement)) toggleRef.current?.focus();
    sidebar.inert = hidden;
    content.inert = narrow && open;
    return () => { sidebar.inert = false; content.inert = false; };
  }, [narrow, open]);
  useFocusScope(sidebarRef, narrow && open, () => setOpen(false), { returnFocus: toggleRef });
  return { open, setOpen, narrow, sidebarRef, contentRef, toggleRef };
}
