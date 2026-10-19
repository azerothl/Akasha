// @vitest-environment jsdom
import { createRef, forwardRef, useImperativeHandle } from "react";
import { act, render, screen, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { deferPanel } from "./deferPanel";
import { I18nProvider } from "../useI18n";

it("forwards an imperative ref after a delayed lazy import and preserves ordinary props", async () => {
  const handle = { setJsonData: vi.fn() };
  const Panel = forwardRef<typeof handle, { label: string }>((props, ref) => {
    useImperativeHandle(ref, () => handle, []);
    return <p>{props.label}</p>;
  });
  let resolve!: (value: { default: typeof Panel }) => void;
  const Deferred = deferPanel(() => new Promise<{ default: typeof Panel }>(done => { resolve = done; }));
  const ref = createRef<typeof handle>();
  const view = render(<I18nProvider><Deferred label="Loaded graph" ref={ref} /></I18nProvider>);
  expect(ref.current).toBeNull();
  await act(async () => { resolve({ default: Panel }); });
  await waitFor(() => expect(ref.current).toBe(handle));
  expect(screen.getByText("Loaded graph")).toBeTruthy();
  ref.current!.setJsonData({ nodes: [] });
  expect(handle.setJsonData).toHaveBeenCalledWith({ nodes: [] });
  view.unmount();
  expect(ref.current).toBeNull();
});
