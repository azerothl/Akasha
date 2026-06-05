import { Fragment } from "react";
import { NAV_GROUP_LABEL_KEYS, NAV_ITEMS, type NavGroupId, type Tab } from "../navigation/types";
import { NavIcon } from "./NavIcon";
import { Tooltip } from "./Tooltip";

type Props = {
  tab: Tab;
  setTab: (t: Tab) => void;
  t: (key: string) => string;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
};

const GROUP_ORDER: NavGroupId[] = ["primary", "workspace", "operations", "data", "help"];

export function AppNavigation({ tab, setTab, t, collapsed, onToggleCollapse }: Props) {
  const collapseBtn = (
    <button type="button" className="sidebar-nav-collapse-btn" onClick={onToggleCollapse} aria-label={t("nav.toggle_sidebar")}>
      {collapsed ? "▶" : "◀"}
    </button>
  );

  return (
    <nav className={`sidebar-nav-grouped ${collapsed ? "sidebar-nav-collapsed" : ""}`} role="tablist" aria-label={t("nav.main")}>
      {onToggleCollapse ? (
        collapsed ? (
          <Tooltip content={t("nav.toggle_sidebar")} placement="right" className="sidebar-nav-tooltip">
            {collapseBtn}
          </Tooltip>
        ) : (
          collapseBtn
        )
      ) : null}
      {GROUP_ORDER.map((group) => {
        const items = NAV_ITEMS.filter((n) => n.group === group);
        if (items.length === 0) return null;
        return (
          <div key={group} className="sidebar-nav-group">
            {!collapsed ? <p className="sidebar-nav-group-label">{t(NAV_GROUP_LABEL_KEYS[group])}</p> : null}
            {items.map((item) => {
              const label = t(item.labelKey);
              const tip = item.shortcut ? `${label} (${item.shortcut})` : label;
              const button = (
                <button
                  role="tab"
                  aria-selected={tab === item.id}
                  aria-controls={`panel-${item.id}`}
                  id={`tab-${item.id}`}
                  className={tab === item.id ? "active" : ""}
                  onClick={() => setTab(item.id)}
                  aria-label={label}
                >
                  {collapsed ? (
                    <span className="sidebar-nav-icon" aria-hidden>
                      <NavIcon id={item.id} />
                    </span>
                  ) : (
                    label
                  )}
                </button>
              );

              if (collapsed) {
                return (
                  <Tooltip key={item.id} content={tip} placement="right" className="sidebar-nav-tooltip">
                    {button}
                  </Tooltip>
                );
              }

              return <Fragment key={item.id}>{button}</Fragment>;
            })}
          </div>
        );
      })}
    </nav>
  );
}
