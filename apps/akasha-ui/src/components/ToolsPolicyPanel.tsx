import { useCallback, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { InfoTip } from "./Tooltip";

const DAEMON_PORT = 3876;

export type ToolsPolicy = {
  allowed_read_paths: string[];
  allowed_write_paths: string[];
  allowed_commands: string[];
  blocked_commands: string[];
  command_timeout_secs: number;
  allowed_web_domains: string[];
  blocked_web_domains: string[];
  web_search_enabled: boolean;
  search_provider: string | null;
  search_fallback_chain: string[];
  searxng_url: string | null;
  web_crawl_enabled: boolean;
  cloudflare_account_id: string | null;
  tool_profiles: Record<string, string[]>;
  default_profile: string | null;
  allowed_skill_install_hosts: string[] | null;
  require_approval: string[];
  allowed_device_interfaces: string[];
  blocked_device_interfaces: string[];
  browser_enabled: boolean;
  browser_allowed_domains: string[];
  browser_blocked_domains: string[];
  browser_headless: boolean;
  browser_action_timeout_secs: number;
  browser_session_timeout_secs: number;
  run_command_default_cwd_workspace: boolean;
  mcp_servers: Record<
    string,
    {
      enabled?: boolean | null;
      allowed_tools: string[];
      blocked_tools: string[];
      max_calls_per_task?: number | null;
    }
  >;
  mcp_max_calls_per_task: number | null;
};

const EMPTY_POLICY: ToolsPolicy = {
  allowed_read_paths: [],
  allowed_write_paths: [],
  allowed_commands: [],
  blocked_commands: [],
  command_timeout_secs: 60,
  allowed_web_domains: [],
  blocked_web_domains: [],
  web_search_enabled: false,
  search_provider: null,
  search_fallback_chain: [],
  searxng_url: null,
  web_crawl_enabled: false,
  cloudflare_account_id: null,
  tool_profiles: {},
  default_profile: null,
  allowed_skill_install_hosts: null,
  require_approval: [],
  allowed_device_interfaces: [],
  blocked_device_interfaces: [],
  browser_enabled: false,
  browser_allowed_domains: [],
  browser_blocked_domains: [],
  browser_headless: true,
  browser_action_timeout_secs: 30,
  browser_session_timeout_secs: 300,
  run_command_default_cwd_workspace: false,
  mcp_servers: {},
  mcp_max_calls_per_task: null,
};

type Props = {
  t: (key: string) => string;
};

function normalizePolicy(raw: unknown): ToolsPolicy {
  const p = (raw && typeof raw === "object" ? raw : {}) as Partial<ToolsPolicy>;
  return {
    ...EMPTY_POLICY,
    ...p,
    allowed_read_paths: Array.isArray(p.allowed_read_paths) ? p.allowed_read_paths.map(String) : [],
    allowed_write_paths: Array.isArray(p.allowed_write_paths) ? p.allowed_write_paths.map(String) : [],
    allowed_commands: Array.isArray(p.allowed_commands) ? p.allowed_commands.map(String) : [],
    blocked_commands: Array.isArray(p.blocked_commands) ? p.blocked_commands.map(String) : [],
    allowed_web_domains: Array.isArray(p.allowed_web_domains) ? p.allowed_web_domains.map(String) : [],
    blocked_web_domains: Array.isArray(p.blocked_web_domains) ? p.blocked_web_domains.map(String) : [],
    search_fallback_chain: Array.isArray(p.search_fallback_chain) ? p.search_fallback_chain.map(String) : [],
    require_approval: Array.isArray(p.require_approval) ? p.require_approval.map(String) : [],
    allowed_device_interfaces: Array.isArray(p.allowed_device_interfaces)
      ? p.allowed_device_interfaces.map(String)
      : [],
    blocked_device_interfaces: Array.isArray(p.blocked_device_interfaces)
      ? p.blocked_device_interfaces.map(String)
      : [],
    browser_allowed_domains: Array.isArray(p.browser_allowed_domains) ? p.browser_allowed_domains.map(String) : [],
    browser_blocked_domains: Array.isArray(p.browser_blocked_domains) ? p.browser_blocked_domains.map(String) : [],
    allowed_skill_install_hosts: Array.isArray(p.allowed_skill_install_hosts)
      ? p.allowed_skill_install_hosts.map(String)
      : p.allowed_skill_install_hosts === null
        ? null
        : null,
    tool_profiles:
      p.tool_profiles && typeof p.tool_profiles === "object" && !Array.isArray(p.tool_profiles)
        ? Object.fromEntries(
            Object.entries(p.tool_profiles).map(([k, v]) => [
              k,
              Array.isArray(v) ? v.map(String) : [],
            ]),
          )
        : {},
    mcp_servers:
      p.mcp_servers && typeof p.mcp_servers === "object" && !Array.isArray(p.mcp_servers)
        ? (p.mcp_servers as ToolsPolicy["mcp_servers"])
        : {},
    command_timeout_secs: typeof p.command_timeout_secs === "number" ? p.command_timeout_secs : 60,
    browser_action_timeout_secs:
      typeof p.browser_action_timeout_secs === "number" ? p.browser_action_timeout_secs : 30,
    browser_session_timeout_secs:
      typeof p.browser_session_timeout_secs === "number" ? p.browser_session_timeout_secs : 300,
    web_search_enabled: Boolean(p.web_search_enabled),
    web_crawl_enabled: Boolean(p.web_crawl_enabled),
    browser_enabled: Boolean(p.browser_enabled),
    browser_headless: p.browser_headless !== false,
    run_command_default_cwd_workspace: Boolean(p.run_command_default_cwd_workspace),
    search_provider: typeof p.search_provider === "string" ? p.search_provider : null,
    searxng_url: typeof p.searxng_url === "string" ? p.searxng_url : null,
    cloudflare_account_id: typeof p.cloudflare_account_id === "string" ? p.cloudflare_account_id : null,
    default_profile: typeof p.default_profile === "string" ? p.default_profile : null,
    mcp_max_calls_per_task: typeof p.mcp_max_calls_per_task === "number" ? p.mcp_max_calls_per_task : null,
  };
}

function PolicyFieldLabel({ label, hintKey, t }: { label: string; hintKey?: string; t: Props["t"] }) {
  return (
    <>
      {label}
      {hintKey ? <InfoTip label={label} content={t(hintKey)} /> : null}
    </>
  );
}

function PolicySection({
  titleKey,
  helpKey,
  tipKey,
  children,
  t,
  defaultOpen,
}: {
  titleKey: string;
  helpKey?: string;
  tipKey?: string;
  children: ReactNode;
  t: Props["t"];
  defaultOpen?: boolean;
}) {
  return (
    <details className="tools-policy-section" open={defaultOpen}>
      <summary className="tools-policy-summary">
        {t(titleKey)}
        {tipKey ? <InfoTip label={t(titleKey)} content={t(tipKey)} /> : null}
      </summary>
      {helpKey ? <p className="tools-policy-section-help settings-doc muted">{t(helpKey)}</p> : null}
      {children}
    </details>
  );
}

function StringListEditor({
  items,
  onChange,
  placeholder,
  addLabel,
}: {
  items: string[];
  onChange: (next: string[]) => void;
  placeholder: string;
  addLabel: string;
}) {
  return (
    <div className="tools-policy-list-editor">
      <ul className="settings-string-list">
        {items.map((item, i) => (
          <li key={i} className="settings-list-item">
            <input
              type="text"
              className="settings-input"
              value={item}
              placeholder={placeholder}
              onChange={(e) => {
                const next = [...items];
                next[i] = e.target.value;
                onChange(next);
              }}
            />
            <button
              type="button"
              className="settings-list-item-delete"
              onClick={() => onChange(items.filter((_, j) => j !== i))}
              aria-label="Remove"
            >
              ×
            </button>
          </li>
        ))}
      </ul>
      <button type="button" className="btn-secondary" onClick={() => onChange([...items, ""])}>
        {addLabel}
      </button>
    </div>
  );
}

export function ToolsPolicyPanel({ t }: Props) {
  const [policy, setPolicy] = useState<ToolsPolicy>(EMPTY_POLICY);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [fileExists, setFileExists] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    setMsg(null);
    try {
      const json = await invoke<{ policy?: unknown; exists?: boolean }>("get_tools_policy", {
        port: DAEMON_PORT,
      });
      setPolicy(normalizePolicy(json?.policy));
      setFileExists(json?.exists !== false);
    } catch (e) {
      setMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    setSaving(true);
    setMsg(null);
    try {
      const cleaned: ToolsPolicy = {
        ...policy,
        allowed_read_paths: policy.allowed_read_paths.map((s) => s.trim()).filter(Boolean),
        allowed_write_paths: policy.allowed_write_paths.map((s) => s.trim()).filter(Boolean),
        allowed_commands: policy.allowed_commands.map((s) => s.trim()).filter(Boolean),
        blocked_commands: policy.blocked_commands.map((s) => s.trim()).filter(Boolean),
        allowed_web_domains: policy.allowed_web_domains.map((s) => s.trim()).filter(Boolean),
        blocked_web_domains: policy.blocked_web_domains.map((s) => s.trim()).filter(Boolean),
        browser_allowed_domains: policy.browser_allowed_domains.map((s) => s.trim()).filter(Boolean),
        browser_blocked_domains: policy.browser_blocked_domains.map((s) => s.trim()).filter(Boolean),
        require_approval: policy.require_approval.map((s) => s.trim()).filter(Boolean),
        allowed_device_interfaces: policy.allowed_device_interfaces.map((s) => s.trim()).filter(Boolean),
        blocked_device_interfaces: policy.blocked_device_interfaces.map((s) => s.trim()).filter(Boolean),
        search_provider: policy.search_provider?.trim() || null,
        searxng_url: policy.searxng_url?.trim() || null,
        cloudflare_account_id: policy.cloudflare_account_id?.trim() || null,
        default_profile: policy.default_profile?.trim() || null,
        allowed_skill_install_hosts: policy.allowed_skill_install_hosts
          ? policy.allowed_skill_install_hosts.map((s) => s.trim()).filter(Boolean)
          : null,
      };
      const result = await invoke<{ ok?: boolean; reloaded?: boolean }>("post_tools_policy", {
        body: cleaned,
        port: DAEMON_PORT,
      });
      setFileExists(true);
      setMsg(
        result?.reloaded
          ? t("tools_policy.saved_reloaded")
          : t("tools_policy.saved"),
      );
    } catch (e) {
      setMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const profileNames = Object.keys(policy.tool_profiles);
  const mcpIds = Object.keys(policy.mcp_servers);

  if (loading) {
    return <p className="panel-loading" aria-busy="true">{t("common.loading")}</p>;
  }

  return (
    <div className="tools-policy-panel">
      <p className="settings-doc muted">{t("tools_policy.desc")}</p>
      {!fileExists ? (
        <p className="settings-doc muted">{t("tools_policy.missing_file")}</p>
      ) : null}

      <PolicySection titleKey="tools_policy.section_paths" helpKey="tools_policy.section_paths_help" t={t} defaultOpen>
        <dl className="settings-list">
          <dt>
            <PolicyFieldLabel label={t("tools_policy.allowed_read_paths")} hintKey="tools_policy.allowed_read_paths_hint" t={t} />
          </dt>
          <dd>
            <StringListEditor
              items={policy.allowed_read_paths}
              onChange={(v) => setPolicy((p) => ({ ...p, allowed_read_paths: v }))}
              placeholder="."
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt>
            <PolicyFieldLabel label={t("tools_policy.allowed_write_paths")} hintKey="tools_policy.allowed_write_paths_hint" t={t} />
          </dt>
          <dd>
            <StringListEditor
              items={policy.allowed_write_paths}
              onChange={(v) => setPolicy((p) => ({ ...p, allowed_write_paths: v }))}
              placeholder="."
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_commands" helpKey="tools_policy.section_commands_help" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.allowed_commands")} hintKey="tools_policy.allowed_commands_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.allowed_commands}
              onChange={(v) => setPolicy((p) => ({ ...p, allowed_commands: v }))}
              placeholder="git"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.blocked_commands")} hintKey="tools_policy.blocked_commands_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.blocked_commands}
              onChange={(v) => setPolicy((p) => ({ ...p, blocked_commands: v }))}
              placeholder="rm"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.command_timeout_secs")} hintKey="tools_policy.command_timeout_secs_hint" t={t} /></dt>
          <dd>
            <input
              type="number"
              className="settings-input"
              min={1}
              value={policy.command_timeout_secs}
              onChange={(e) =>
                setPolicy((p) => ({
                  ...p,
                  command_timeout_secs: Math.max(1, parseInt(e.target.value, 10) || 60),
                }))
              }
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.run_command_default_cwd_workspace")} hintKey="tools_policy.run_command_default_cwd_workspace_hint" t={t} /></dt>
          <dd>
            <label className="settings-checkbox-label">
              <input
                type="checkbox"
                checked={policy.run_command_default_cwd_workspace}
                onChange={(e) =>
                  setPolicy((p) => ({ ...p, run_command_default_cwd_workspace: e.target.checked }))
                }
              />
              {t("tools_policy.run_command_default_cwd_workspace_label")}
            </label>
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_web" helpKey="tools_policy.section_web_help" tipKey="tools_policy.section_web_tip" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.web_search_enabled")} hintKey="tools_policy.web_search_enabled_hint" t={t} /></dt>
          <dd>
            <label className="settings-checkbox-label">
              <input
                type="checkbox"
                checked={policy.web_search_enabled}
                onChange={(e) => setPolicy((p) => ({ ...p, web_search_enabled: e.target.checked }))}
              />
              {t("tools_policy.web_search_enabled_label")}
            </label>
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.web_crawl_enabled")} hintKey="tools_policy.web_crawl_enabled_hint" t={t} /></dt>
          <dd>
            <label className="settings-checkbox-label">
              <input
                type="checkbox"
                checked={policy.web_crawl_enabled}
                onChange={(e) => setPolicy((p) => ({ ...p, web_crawl_enabled: e.target.checked }))}
              />
              {t("tools_policy.web_crawl_enabled_label")}
            </label>
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.search_provider")} hintKey="tools_policy.search_provider_hint" t={t} /></dt>
          <dd>
            <input
              type="text"
              className="settings-input"
              value={policy.search_provider ?? ""}
              placeholder="auto"
              onChange={(e) => setPolicy((p) => ({ ...p, search_provider: e.target.value || null }))}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.searxng_url")} hintKey="tools_policy.searxng_url_hint" t={t} /></dt>
          <dd>
            <input
              type="text"
              className="settings-input"
              value={policy.searxng_url ?? ""}
              placeholder="https://searx.be"
              onChange={(e) => setPolicy((p) => ({ ...p, searxng_url: e.target.value || null }))}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.allowed_web_domains")} hintKey="tools_policy.allowed_web_domains_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.allowed_web_domains}
              onChange={(v) => setPolicy((p) => ({ ...p, allowed_web_domains: v }))}
              placeholder="example.com"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.blocked_web_domains")} hintKey="tools_policy.blocked_web_domains_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.blocked_web_domains}
              onChange={(v) => setPolicy((p) => ({ ...p, blocked_web_domains: v }))}
              placeholder="evil.com"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_browser" helpKey="tools_policy.section_browser_help" tipKey="tools_policy.section_browser_tip" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.browser_enabled")} hintKey="tools_policy.browser_enabled_hint" t={t} /></dt>
          <dd>
            <label className="settings-checkbox-label">
              <input
                type="checkbox"
                checked={policy.browser_enabled}
                onChange={(e) => setPolicy((p) => ({ ...p, browser_enabled: e.target.checked }))}
              />
              {t("tools_policy.browser_enabled_label")}
            </label>
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.browser_headless")} hintKey="tools_policy.browser_headless_hint" t={t} /></dt>
          <dd>
            <label className="settings-checkbox-label">
              <input
                type="checkbox"
                checked={policy.browser_headless}
                onChange={(e) => setPolicy((p) => ({ ...p, browser_headless: e.target.checked }))}
              />
              {t("tools_policy.browser_headless_label")}
            </label>
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.browser_allowed_domains")} hintKey="tools_policy.browser_allowed_domains_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.browser_allowed_domains}
              onChange={(v) => setPolicy((p) => ({ ...p, browser_allowed_domains: v }))}
              placeholder="localhost"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.browser_blocked_domains")} hintKey="tools_policy.browser_blocked_domains_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.browser_blocked_domains}
              onChange={(v) => setPolicy((p) => ({ ...p, browser_blocked_domains: v }))}
              placeholder=""
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_devices" helpKey="tools_policy.section_devices_help" tipKey="tools_policy.section_devices_tip" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.allowed_device_interfaces")} hintKey="tools_policy.allowed_device_interfaces_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.allowed_device_interfaces}
              onChange={(v) => setPolicy((p) => ({ ...p, allowed_device_interfaces: v }))}
              placeholder="local_media"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.blocked_device_interfaces")} hintKey="tools_policy.blocked_device_interfaces_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.blocked_device_interfaces}
              onChange={(v) => setPolicy((p) => ({ ...p, blocked_device_interfaces: v }))}
              placeholder="usb"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_profiles" helpKey="tools_policy.section_profiles_help" tipKey="tools_policy.section_profiles_tip" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.default_profile")} hintKey="tools_policy.default_profile_hint" t={t} /></dt>
          <dd>
            <select
              className="settings-theme-select"
              value={policy.default_profile ?? ""}
              onChange={(e) =>
                setPolicy((p) => ({ ...p, default_profile: e.target.value.trim() || null }))
              }
            >
              <option value="">—</option>
              {profileNames.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          </dd>
          {profileNames.map((name) => (
            <div key={name} className="tools-policy-profile-block">
              <dt>{t("tools_policy.profile_tools")}: {name}</dt>
              <dd>
                <StringListEditor
                  items={policy.tool_profiles[name] ?? []}
                  onChange={(tools) =>
                    setPolicy((p) => ({
                      ...p,
                      tool_profiles: { ...p.tool_profiles, [name]: tools },
                    }))
                  }
                  placeholder="read_file"
                  addLabel={t("tools_policy.add_entry")}
                />
                <button
                  type="button"
                  className="btn-secondary"
                  onClick={() => {
                    const next = { ...policy.tool_profiles };
                    delete next[name];
                    setPolicy((p) => ({
                      ...p,
                      tool_profiles: next,
                      default_profile: p.default_profile === name ? null : p.default_profile,
                    }));
                  }}
                >
                  {t("tools_policy.remove_profile")}
                </button>
              </dd>
            </div>
          ))}
          <dd>
            <button
              type="button"
              className="btn-secondary"
              onClick={() => {
                const base = "profile";
                let n = 1;
                let id = `${base}_${n}`;
                while (policy.tool_profiles[id]) {
                  n += 1;
                  id = `${base}_${n}`;
                }
                setPolicy((p) => ({
                  ...p,
                  tool_profiles: { ...p.tool_profiles, [id]: [] },
                }));
              }}
            >
              {t("tools_policy.add_profile")}
            </button>
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_approval" helpKey="tools_policy.section_approval_help" tipKey="tools_policy.section_approval_tip" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.require_approval")} hintKey="tools_policy.require_approval_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.require_approval}
              onChange={(v) => setPolicy((p) => ({ ...p, require_approval: v }))}
              placeholder="write_file"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
        </dl>
      </PolicySection>

      <PolicySection titleKey="tools_policy.section_skills_mcp" helpKey="tools_policy.section_skills_mcp_help" t={t}>
        <dl className="settings-list">
          <dt><PolicyFieldLabel label={t("tools_policy.allowed_skill_install_hosts")} hintKey="tools_policy.allowed_skill_install_hosts_hint" t={t} /></dt>
          <dd>
            <StringListEditor
              items={policy.allowed_skill_install_hosts ?? []}
              onChange={(v) => setPolicy((p) => ({ ...p, allowed_skill_install_hosts: v }))}
              placeholder="github.com"
              addLabel={t("tools_policy.add_entry")}
            />
          </dd>
          <dt><PolicyFieldLabel label={t("tools_policy.mcp_max_calls_per_task")} hintKey="tools_policy.mcp_max_calls_per_task_hint" t={t} /></dt>
          <dd>
            <input
              type="number"
              className="settings-input"
              min={0}
              value={policy.mcp_max_calls_per_task ?? ""}
              onChange={(e) => {
                const raw = e.target.value.trim();
                setPolicy((p) => ({
                  ...p,
                  mcp_max_calls_per_task: raw ? Math.max(0, parseInt(raw, 10) || 0) : null,
                }));
              }}
            />
          </dd>
          {mcpIds.map((id) => {
            const srv = policy.mcp_servers[id] ?? { allowed_tools: [], blocked_tools: [] };
            return (
              <div key={id} className="tools-policy-mcp-block">
                <dt>MCP: {id}</dt>
                <dd>
                  <label className="settings-checkbox-label">
                    <input
                      type="checkbox"
                      checked={srv.enabled !== false}
                      onChange={(e) =>
                        setPolicy((p) => ({
                          ...p,
                          mcp_servers: {
                            ...p.mcp_servers,
                            [id]: { ...srv, enabled: e.target.checked },
                          },
                        }))
                      }
                    />
                    {t("tools_policy.mcp_enabled")}
                  </label>
                  <StringListEditor
                    items={srv.allowed_tools ?? []}
                    onChange={(tools) =>
                      setPolicy((p) => ({
                        ...p,
                        mcp_servers: { ...p.mcp_servers, [id]: { ...srv, allowed_tools: tools } },
                      }))
                    }
                    placeholder="*"
                    addLabel={t("tools_policy.add_entry")}
                  />
                  <button
                    type="button"
                    className="btn-secondary"
                    onClick={() => {
                      const next = { ...policy.mcp_servers };
                      delete next[id];
                      setPolicy((p) => ({ ...p, mcp_servers: next }));
                    }}
                  >
                    {t("tools_policy.remove_mcp")}
                  </button>
                </dd>
              </div>
            );
          })}
          <dd>
            <button
              type="button"
              className="btn-secondary"
              onClick={() => {
                const id = prompt(t("tools_policy.mcp_id_prompt"));
                if (!id?.trim()) return;
                setPolicy((p) => ({
                  ...p,
                  mcp_servers: {
                    ...p.mcp_servers,
                    [id.trim()]: { enabled: true, allowed_tools: ["*"], blocked_tools: [] },
                  },
                }));
              }}
            >
              {t("tools_policy.add_mcp")}
            </button>
          </dd>
        </dl>
      </PolicySection>

      <div className="settings-row-actions">
        <button type="button" className="btn-primary" disabled={saving} onClick={() => void save()}>
          {saving ? t("common.loading") : t("tools_policy.save")}
        </button>
        <button type="button" className="btn-secondary" disabled={loading} onClick={() => void load()}>
          {t("tools_policy.reload_from_disk")}
        </button>
      </div>
      {msg ? <p className="muted">{msg}</p> : null}
    </div>
  );
}
