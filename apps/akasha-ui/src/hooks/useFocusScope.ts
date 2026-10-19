import { useLayoutEffect, useRef, type RefObject } from "react";

const focusable = 'button:not(:disabled), a[href], summary, input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

/** Contains keyboard focus for an open modal or navigation drawer. */
export function useFocusScope(
  ref: RefObject<HTMLElement>,
  active: boolean,
  onEscape: () => void,
  options: { inertOutside?: boolean; returnFocus?: RefObject<HTMLElement> } = {},
) {
  const escapeRef = useRef(onEscape);
  escapeRef.current = onEscape;
  const returnRef = options.returnFocus;
  const inertOutside = options.inertOutside;
  useLayoutEffect(() => {
    const scope = ref.current;
    if (!active || !scope) return;
    const previous = document.activeElement as HTMLElement | null;
    const inerted: Array<[HTMLElement, boolean]> = [];
    if (inertOutside) {
      let branch: HTMLElement = scope;
      while (branch.parentElement) {
        for (const sibling of Array.from(branch.parentElement.children)) {
          if (sibling !== branch && sibling instanceof HTMLElement) {
            inerted.push([sibling, sibling.inert]);
            sibling.inert = true;
          }
        }
        branch = branch.parentElement;
        if (branch === document.body) break;
      }
    }
    const targets = () => Array.from(scope.querySelectorAll<HTMLElement>(focusable))
      .filter(el => {
        if (!el.getClientRects().length || el.closest("[inert]")) return false;
        // Closed details can still expose descendant layout boxes in some WebViews.
        let parent = el.parentElement;
        while (parent && parent !== scope) {
          if (parent.tagName === "DETAILS" && !parent.hasAttribute("open") && el !== parent.querySelector("summary")) return false;
          parent = parent.parentElement;
        }
        return true;
      });
    const focusFirst = () => (scope.querySelector<HTMLElement>("[data-initial-focus]") ?? targets()[0] ?? scope).focus();
    focusFirst();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        escapeRef.current();
      }
      if (event.key !== "Tab") return;
      const items = targets();
      const first = items[0], last = items[items.length - 1];
      const focused = document.activeElement;
      if (!first) { event.preventDefault(); scope.focus(); return; }
      if (event.shiftKey && (focused === first || !items.includes(focused as HTMLElement))) {
        event.preventDefault(); last.focus();
      } else if (!event.shiftKey && (focused === last || !scope.contains(focused))) {
        event.preventDefault(); first.focus();
      }
    };
    const focusin = (event: FocusEvent) => {
      if (!scope.contains(event.target as Node)) focusFirst();
    };
    document.addEventListener("keydown", keydown, true);
    document.addEventListener("focusin", focusin);
    return () => {
      document.removeEventListener("keydown", keydown, true);
      document.removeEventListener("focusin", focusin);
      for (const [el, wasInert] of inerted) el.inert = wasInert;
      const target = returnRef?.current ?? previous;
      if (target?.isConnected && target.getClientRects().length && !target.closest("[inert]")) target.focus();
    };
  }, [active, ref, inertOutside, returnRef]);
}
