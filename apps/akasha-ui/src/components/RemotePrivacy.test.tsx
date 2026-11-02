// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RemoteConsentPicker, RemotePermissions } from "./RemotePrivacy";

afterEach(cleanup);
const remote = { name: "Example", endpoint: "https://model.example/v1", recipient: "model:example", local: false, data_classes: ["message", "documents"] };
const privacy = { services: [remote, { ...remote, recipient: "local", local: true }], search_services: [], memory: { state: "locked", encrypted_at_rest: false }, grants: [{ ...remote, scope: "task:sample", revoked: false, expires_at: "2099-01-01T00:00:00Z" }] };
it("requires a separate unchecked choice and displays the recipient and data scope", async () => {
  const fetchEndpoint = vi.fn().mockResolvedValue({ ok: true, status: 200, text: JSON.stringify(privacy) });
  const change = vi.fn();
  render(<RemoteConsentPicker fetchEndpoint={fetchEndpoint} locale="fr" value={[]} onChange={change} disabled={false} />);
  const checkbox = await screen.findByRole("checkbox", { hidden: true });
  expect((checkbox as HTMLInputElement).checked).toBe(false);
  expect(screen.getByText(/Peut recevoir votre message/)).toBeDefined();
  fireEvent.click(checkbox);
  expect(change).toHaveBeenCalledWith([{ recipient: remote.recipient, data_classes: remote.data_classes }]);
  expect(fetchEndpoint).toHaveBeenCalledTimes(1);
});
it("revokes the actual grant and refreshes the effective state", async () => {
  const fetchEndpoint = vi.fn().mockImplementation((path: string) => Promise.resolve({ ok: true, status: 200, text: JSON.stringify(path.endsWith("revoke") ? { revoked: true } : privacy) }));
  render(<RemotePermissions fetchEndpoint={fetchEndpoint} locale="fr" />);
  fireEvent.click(await screen.findByRole("button", { name: "Révoquer" }));
  await waitFor(() => expect(fetchEndpoint).toHaveBeenCalledWith("/api/permissions/remote/revoke", { method: "POST", body: JSON.stringify({ scope: "task:sample", recipient: "model:example" }) }));
  expect(screen.getByText(/Mémoire verrouillée/)).toBeDefined();
});
it("does not offer remote sending when the effective state cannot be read", async () => {
  const fetchEndpoint = vi.fn().mockRejectedValue(new Error("offline"));
  render(<RemoteConsentPicker fetchEndpoint={fetchEndpoint} locale="fr" value={[]} onChange={vi.fn()} disabled={false} />);
  expect(await screen.findByRole("alert")).toBeDefined();
  expect(screen.queryByRole("checkbox")).toBeNull();
});
it("requires a click to grant a newly refused recipient for the original task", async () => {
  const request = { scope: "task:sample", recipient: "web:https://source.example", data_classes: ["web_url"] };
  const fetchEndpoint = vi.fn().mockResolvedValue({ ok: true, status: 200, text: JSON.stringify({ ...privacy, requests: [request] }) });
  render(<RemotePermissions fetchEndpoint={fetchEndpoint} locale="fr" />);
  const allow = await screen.findByRole("button", { name: "Autoriser pour cette tâche" });
  expect(fetchEndpoint).toHaveBeenCalledTimes(1);
  fireEvent.click(allow);
  await waitFor(() => expect(fetchEndpoint).toHaveBeenCalledWith("/api/permissions/remote", { method: "POST", body: JSON.stringify({ confirmed: true, scope: request.scope, consents: [{ recipient: request.recipient, data_classes: request.data_classes }] }) }));
});
