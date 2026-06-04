export type Tab =
  | "chat"
  | "compare"
  | "research"
  | "cookbook"
  | "scheduled"
  | "router"
  | "mission"
  | "docs"
  | "tasks"
  | "calendar"
  | "memory"
  | "settings";

export type NavGroupId = "primary" | "workspace" | "operations" | "data" | "help";

export type NavItem = {
  id: Tab;
  labelKey: string;
  group: NavGroupId;
  shortcut?: string;
};

export const NAV_ITEMS: NavItem[] = [
  { id: "chat", labelKey: "tabs.chat", group: "primary", shortcut: "1" },
  { id: "compare", labelKey: "tabs.compare", group: "workspace", shortcut: "2" },
  { id: "research", labelKey: "tabs.research", group: "workspace", shortcut: "3" },
  { id: "cookbook", labelKey: "tabs.cookbook", group: "workspace" },
  { id: "tasks", labelKey: "tabs.tasks", group: "operations", shortcut: "4" },
  { id: "scheduled", labelKey: "tabs.scheduled", group: "operations" },
  { id: "router", labelKey: "tabs.router", group: "operations", shortcut: "5" },
  { id: "mission", labelKey: "tabs.mission", group: "operations", shortcut: "9" },
  { id: "calendar", labelKey: "tabs.calendar", group: "data", shortcut: "6" },
  { id: "memory", labelKey: "tabs.memory", group: "data", shortcut: "7" },
  { id: "docs", labelKey: "tabs.docs", group: "help", shortcut: "8" },
  { id: "settings", labelKey: "tabs.settings", group: "help" },
];

export const NAV_GROUP_LABEL_KEYS: Record<NavGroupId, string> = {
  primary: "nav.group_primary",
  workspace: "nav.group_workspace",
  operations: "nav.group_operations",
  data: "nav.group_data",
  help: "nav.group_help",
};

export function tabFromHash(hash: string): Tab | null {
  const raw = hash.replace(/^#\/?/, "").split("?")[0]?.trim() ?? "";
  const id = raw.split("/")[0] as Tab;
  return NAV_ITEMS.some((n) => n.id === id) ? id : null;
}

export function hashForTab(tab: Tab): string {
  return `#/${tab}`;
}
