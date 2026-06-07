import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const DAEMON_PORT = 3876;

export type DeviceCategory = "local" | "network" | "usb" | "other";

export type CatalogDevice = {
  id: string;
  name: string;
};

export type CatalogInterface = {
  id: string;
  category: DeviceCategory;
  supported: boolean;
  devices: CatalogDevice[];
};

const FALLBACK_CATALOG: CatalogInterface[] = [
  {
    id: "local_media",
    category: "local",
    supported: true,
    devices: [
      { id: "camera", name: "Camera" },
      { id: "microphone", name: "Microphone" },
      { id: "speaker", name: "Speaker" },
    ],
  },
  {
    id: "synthetic_input",
    category: "local",
    supported: true,
    devices: [
      { id: "keyboard", name: "Keyboard" },
      { id: "mouse", name: "Mouse" },
    ],
  },
  {
    id: "system",
    category: "local",
    supported: true,
    devices: [{ id: "printer", name: "System printers" }],
  },
  { id: "network", category: "network", supported: false, devices: [] },
  { id: "usb", category: "usb", supported: false, devices: [] },
  { id: "*", category: "other", supported: true, devices: [] },
];

const CATEGORY_ORDER: DeviceCategory[] = ["local", "network", "usb", "other"];

function normalizeCatalog(raw: unknown): CatalogInterface[] {
  if (!raw || typeof raw !== "object") return FALLBACK_CATALOG;
  const interfaces = (raw as { interfaces?: unknown }).interfaces;
  if (!Array.isArray(interfaces)) return FALLBACK_CATALOG;
  const parsed: CatalogInterface[] = [];
  for (const item of interfaces) {
    if (!item || typeof item !== "object") continue;
    const o = item as Record<string, unknown>;
    const id = typeof o.id === "string" ? o.id : "";
    if (!id) continue;
    const categoryRaw = typeof o.category === "string" ? o.category : "other";
    const category = (CATEGORY_ORDER.includes(categoryRaw as DeviceCategory)
      ? categoryRaw
      : "other") as DeviceCategory;
    const devices = Array.isArray(o.devices)
      ? o.devices
          .map((d) => {
            if (!d || typeof d !== "object") return null;
            const dev = d as Record<string, unknown>;
            const devId = typeof dev.id === "string" ? dev.id : "";
            const name = typeof dev.name === "string" ? dev.name : devId;
            return devId ? { id: devId, name } : null;
          })
          .filter((d): d is CatalogDevice => d !== null)
      : [];
    parsed.push({
      id,
      category,
      supported: o.supported !== false,
      devices,
    });
  }
  return parsed.length > 0 ? parsed : FALLBACK_CATALOG;
}

type Props = {
  items: string[];
  onChange: (next: string[]) => void;
  t: (key: string) => string;
  addLabel: string;
};

export function DeviceInterfaceListEditor({ items, onChange, t, addLabel }: Props) {
  const [catalog, setCatalog] = useState<CatalogInterface[]>(FALLBACK_CATALOG);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [customValue, setCustomValue] = useState("");

  const loadCatalog = useCallback(async () => {
    try {
      const json = await invoke<{ interfaces?: unknown }>("get_device_interfaces", {
        port: DAEMON_PORT,
      });
      setCatalog(normalizeCatalog(json));
    } catch {
      setCatalog(FALLBACK_CATALOG);
    }
  }, []);

  useEffect(() => {
    void loadCatalog();
  }, [loadCatalog]);

  const ifaceLabel = useCallback(
    (id: string) => {
      const key = `tools_policy.device_iface.${id.replace(/\*/g, "all")}`;
      const translated = t(key);
      return translated !== key ? translated : id;
    },
    [t],
  );

  const deviceLabel = useCallback(
    (ifaceId: string, deviceId: string, fallback: string) => {
      const key = `tools_policy.device_item.${ifaceId}.${deviceId}`;
      const translated = t(key);
      return translated !== key ? translated : fallback;
    },
    [t],
  );

  const byCategory = useMemo(() => {
    const map: Record<DeviceCategory, CatalogInterface[]> = {
      local: [],
      network: [],
      usb: [],
      other: [],
    };
    for (const iface of catalog) {
      map[iface.category]?.push(iface);
    }
    return map;
  }, [catalog]);

  const selectedSet = useMemo(() => new Set(items.map((s) => s.trim()).filter(Boolean)), [items]);

  const addInterface = (id: string) => {
    const trimmed = id.trim();
    if (!trimmed || selectedSet.has(trimmed)) return;
    onChange([...items.filter((s) => s.trim()), trimmed]);
    setPickerOpen(false);
    setCustomValue("");
  };

  const removeAt = (index: number) => {
    onChange(items.filter((_, i) => i !== index));
  };

  return (
    <div className="tools-policy-list-editor device-interface-editor">
      <ul className="settings-string-list device-interface-selected">
        {items.map((item, i) =>
          item.trim() ? (
            <li key={`${item}-${i}`} className="device-interface-chip">
              <span className="device-interface-chip-label">{ifaceLabel(item.trim())}</span>
              <code className="device-interface-chip-id">{item.trim()}</code>
              <button
                type="button"
                className="settings-list-item-delete"
                onClick={() => removeAt(i)}
                aria-label={t("tools_policy.remove_entry")}
              >
                ×
              </button>
            </li>
          ) : null,
        )}
      </ul>

      <div className="device-interface-picker-wrap">
        <button
          type="button"
          className="btn-secondary"
          aria-expanded={pickerOpen}
          onClick={() => setPickerOpen((o) => !o)}
        >
          {addLabel}
        </button>

        {pickerOpen ? (
          <div className="device-interface-picker" role="region" aria-label={addLabel}>
            {CATEGORY_ORDER.map((category) => {
              const list = byCategory[category];
              if (!list.length) return null;
              return (
                <section key={category} className="device-interface-category">
                  <h4 className="device-interface-category-title">
                    {t(`tools_policy.device_category.${category}`)}
                  </h4>
                  <ul className="device-interface-options">
                    {list.map((iface) => {
                      const taken = selectedSet.has(iface.id);
                      return (
                        <li key={iface.id} className="device-interface-option">
                          <button
                            type="button"
                            className="device-interface-option-btn"
                            disabled={taken}
                            onClick={() => addInterface(iface.id)}
                          >
                            <span className="device-interface-option-head">
                              <span className="device-interface-option-name">{ifaceLabel(iface.id)}</span>
                              <code className="device-interface-option-id">{iface.id}</code>
                              {!iface.supported ? (
                                <span className="device-interface-badge device-interface-badge--soon">
                                  {t("tools_policy.device_iface_unsupported")}
                                </span>
                              ) : null}
                              {taken ? (
                                <span className="device-interface-badge device-interface-badge--added">
                                  {t("tools_policy.device_iface_added")}
                                </span>
                              ) : null}
                            </span>
                            {iface.devices.length > 0 ? (
                              <ul className="device-interface-devices">
                                {iface.devices.map((dev) => (
                                  <li key={dev.id}>
                                    {deviceLabel(iface.id, dev.id, dev.name)}
                                  </li>
                                ))}
                              </ul>
                            ) : null}
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </section>
              );
            })}

            <div className="device-interface-custom">
              <label className="device-interface-custom-label" htmlFor="device-interface-custom-input">
                {t("tools_policy.device_iface_custom")}
              </label>
              <div className="device-interface-custom-row">
                <input
                  id="device-interface-custom-input"
                  type="text"
                  className="settings-input"
                  value={customValue}
                  placeholder="local_media"
                  onChange={(e) => setCustomValue(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      addInterface(customValue);
                    }
                  }}
                />
                <button
                  type="button"
                  className="btn-secondary"
                  disabled={!customValue.trim() || selectedSet.has(customValue.trim())}
                  onClick={() => addInterface(customValue)}
                >
                  {t("tools_policy.device_iface_custom_add")}
                </button>
              </div>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
