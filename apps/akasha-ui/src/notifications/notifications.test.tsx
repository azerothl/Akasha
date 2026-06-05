// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { NotificationProvider, useNotifications } from "./NotificationContext";
import { renderHook, act } from "@testing-library/react";
import type { ReactNode } from "react";

function wrapper({ children }: { children: ReactNode }) {
  return <NotificationProvider>{children}</NotificationProvider>;
}

describe("NotificationContext", () => {
  it("enqueues and dismisses notifications", () => {
    const { result } = renderHook(() => useNotifications(), { wrapper });

    act(() => {
      result.current.notify({ level: "error", title: "Test error", source: "unit" });
    });

    expect(result.current.notifications).toHaveLength(1);
    expect(result.current.unreadCount).toBe(1);

    const id = result.current.notifications[0]!.id;
    act(() => {
      result.current.dismiss(id);
    });

    expect(result.current.notifications).toHaveLength(0);
    expect(result.current.unreadCount).toBe(0);
  });

  it("marks all read", () => {
    const { result } = renderHook(() => useNotifications(), { wrapper });

    act(() => {
      result.current.notify({ level: "warning", title: "A" });
      result.current.notify({ level: "info", title: "B" });
    });

    expect(result.current.unreadCount).toBe(2);

    act(() => {
      result.current.markAllRead();
    });

    expect(result.current.unreadCount).toBe(0);
    expect(result.current.notifications.every((n) => n.read)).toBe(true);
  });
});
