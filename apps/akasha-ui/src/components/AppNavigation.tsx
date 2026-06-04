import { NAV_GROUP_LABEL_KEYS, NAV_ITEMS, type NavGroupId, type Tab } from "../navigation/types";

type Props = {
  tab: Tab;
  setTab: (t: Tab) => void;
  t: (key: string) => string;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
};

const GROUP_ORDER: NavGroupId[] = ["primary", "workspace", "operations", "data", "help"];

export function AppNavigation({ tab, setTab, t, collapsed, onToggleCollapse }: Props) {
  return (
    <nav className={`sidebar-nav-grouped ${collapsed ? "sidebar-nav-collapsed" : ""}`} role="tablist" aria-label={t("nav.main")}>
      {onToggleCollapse ? (
        <button type="button" className="sidebar-nav-collapse-btn" onClick={onToggleCollapse} aria-label={t("nav.toggle_sidebar")}>
          {collapsed ? "▶" : "◀"}
        </button>
      ) : null}
      {GROUP_ORDER.map((group) => {
        const items = NAV_ITEMS.filter((n) => n.group === group);
        if (items.length === 0) return null;
        return (
          <div key={group} className="sidebar-nav-group">
            {!collapsed ? <p className="sidebar-nav-group-label">{t(NAV_GROUP_LABEL_KEYS[group])}</p> : null}
            {items.map((item) => (
              <button
                key={item.id}
                role="tab"
                aria-selected={tab === item.id}
                aria-controls={`panel-${item.id}`}
                id={`tab-${item.id}`}
                className={tab === item.id ? "active" : ""}
                onClick={() => setTab(item.id)}
                title={item.shortcut ? `${t(item.labelKey)} (${item.shortcut})` : t(item.labelKey)}
              >
                {collapsed ? t(item.labelKey).slice(0, 1) : t(item.labelKey)}
              </button>
            ))}
          </div>
        );
      })}
    </nav>
  );
}
