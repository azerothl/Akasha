import { useEffect, useRef, useState } from "react";
import { useNotifications } from "../notifications/NotificationContext";
import type { Notification } from "../notifications/types";
import { useI18n } from "../useI18n";
import { MonoIcon } from "./MonoIcon";

function formatTime(ts: number, locale: string): string {
  try {
    return new Intl.DateTimeFormat(locale === "fr" ? "fr-FR" : "en-US", {
      hour: "2-digit",
      minute: "2-digit",
      day: "2-digit",
      month: "short",
    }).format(new Date(ts));
  } catch {
    return "";
  }
}

function NotificationItem({
  item,
  locale,
  onRead,
  onDismiss,
}: {
  item: Notification;
  locale: "fr" | "en";
  onRead: (id: string) => void;
  onDismiss: (id: string) => void;
}) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);

  return (
    <li
      className={`notification-center-item notification-center-item--${item.level}${item.read ? "" : " notification-center-item--unread"}`}
      role="alert"
    >
      <div
        className="notification-center-item-head"
        onClick={() => {
          onRead(item.id);
          if (item.detail) setExpanded((e) => !e);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onRead(item.id);
            if (item.detail) setExpanded((x) => !x);
          }
        }}
        role="button"
        tabIndex={0}
      >
        <p className="notification-center-item-title">{item.title}</p>
        <span className="notification-center-item-meta">{formatTime(item.timestamp, locale)}</span>
      </div>
      {item.source ? <p className="notification-center-item-source">{item.source}</p> : null}
      {expanded && item.detail ? (
        <p className="notification-center-item-detail">{item.detail}</p>
      ) : null}
      {!expanded && item.detail ? (
        <p className="notification-center-item-detail" style={{ opacity: 0.7 }}>
          {item.detail.length > 80 ? `${item.detail.slice(0, 80)}…` : item.detail}
        </p>
      ) : null}
      <button
        type="button"
        className="notification-center-item-dismiss"
        onClick={(e) => {
          e.stopPropagation();
          onDismiss(item.id);
        }}
      >
        {t("notifications.dismiss")}
      </button>
    </li>
  );
}

export function NotificationCenter() {
  const { t, locale } = useI18n();
  const { notifications, unreadCount, dismiss, markRead, markAllRead, clearAll } = useNotifications();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  return (
    <div className="notification-center-wrap" ref={wrapRef}>
      <button
        type="button"
        className="notification-center-trigger"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={t("notifications.title")}
      >
        <MonoIcon name="bell" />
        {unreadCount > 0 ? (
          <span className="notification-center-badge" aria-label={`${unreadCount}`}>
            {unreadCount > 99 ? "99+" : unreadCount}
          </span>
        ) : null}
      </button>
      {open ? (
        <div
          className="notification-center-drawer"
          role="dialog"
          aria-label={t("notifications.title")}
        >
          <div className="notification-center-drawer-head">
            <strong>{t("notifications.title")}</strong>
            <div className="notification-center-drawer-actions">
              {notifications.length > 0 ? (
                <>
                  <button type="button" onClick={markAllRead}>
                    {t("notifications.mark_read")}
                  </button>
                  <button type="button" onClick={clearAll}>
                    {t("notifications.clear_all")}
                  </button>
                </>
              ) : null}
              <button type="button" onClick={() => setOpen(false)} aria-label={t("common.close")}>
                ×
              </button>
            </div>
          </div>
          {notifications.length === 0 ? (
            <p className="notification-center-empty">{t("notifications.empty")}</p>
          ) : (
            <ul className="notification-center-list">
              {notifications.map((item) => (
                <NotificationItem
                  key={item.id}
                  item={item}
                  locale={locale}
                  onRead={markRead}
                  onDismiss={dismiss}
                />
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </div>
  );
}

export { formatTime };
