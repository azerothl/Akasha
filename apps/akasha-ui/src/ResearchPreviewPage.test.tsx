// @vitest-environment jsdom
import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ResearchPreviewPage } from "./ResearchPreviewPage";
import { RESEARCH_PREVIEW_STORAGE_PREFIX } from "./researchReportExport";

const html = `<!doctype html><html><body>
  <div class="research-report-print-bar"><button type="button" onclick="window.print()">Enregistrer en PDF</button></div>
  <h1>Facture fictive</h1><p>42 euros</p>
</body></html>`;

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.setItem(RESEARCH_PREVIEW_STORAGE_PREFIX + "print-test", html);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
  localStorage.clear();
  window.history.replaceState(null, "", "/");
});

function setup(autoPrint = true) {
  window.history.replaceState(null, "", `/#/research-preview?key=print-test${autoPrint ? "&print=1" : ""}`);
  const view = render(<StrictMode><ResearchPreviewPage /></StrictMode>);
  const frame = screen.getByTitle("Research report preview") as HTMLIFrameElement;
  // JSDOM does not populate iframe srcdoc; supply the loaded document explicitly.
  frame.contentDocument!.body.innerHTML = html;
  // Simulate the native preview CSP blocking the inline onclick handler.
  frame.contentDocument!.querySelector("button")!.removeAttribute("onclick");
  vi.spyOn(frame.contentWindow!, "focus").mockImplementation(() => {});
  const print = vi.spyOn(frame.contentWindow!, "print").mockImplementation(() => {});
  return { ...view, frame, print };
}

describe("PDF preview automatic printing", () => {
  it("waits for the report frame to load before printing", () => {
    const { frame, print } = setup();
    act(() => vi.advanceTimersByTime(1000));
    expect(print).not.toHaveBeenCalled();
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(400));
    expect(print).toHaveBeenCalledTimes(1);
  });

  it("prints once despite competing load notifications before and after saving", () => {
    const { frame, print } = setup();
    fireEvent.load(frame);
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(400));
    expect(print).toHaveBeenCalledTimes(1);
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(1000));
    expect(print).toHaveBeenCalledTimes(1);
    expect(frame.getAttribute("srcdoc")).toBe(html);
  });

  it("consumes automatic printing before entering the blocking print dialog", () => {
    const { frame, print } = setup();
    print.mockImplementation(() => { fireEvent.load(frame); });
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(1200));
    expect(print).toHaveBeenCalledTimes(1);
  });

  it("cancels a queued print when the preview closes", () => {
    const { frame, print, unmount } = setup();
    fireEvent.load(frame);
    unmount();
    act(() => vi.advanceTimersByTime(1000));
    expect(print).not.toHaveBeenCalled();
  });

  it("does not print an ordinary report preview automatically", () => {
    const { frame, print } = setup(false);
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(1000));
    expect(print).not.toHaveBeenCalled();
    expect(localStorage.getItem(RESEARCH_PREVIEW_STORAGE_PREFIX + "print-test")).toBeNull();
  });

  it("does not reopen a blocked or dismissed print request on another load", () => {
    const { frame, print } = setup();
    print.mockImplementation(() => { throw new Error("Print dialog dismissed"); });
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(400));
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(1000));
    expect(print).toHaveBeenCalledTimes(1);
  });

  it.each([true, false])("allows an explicit reprint even when inline handlers are blocked (autoPrint=%s)", (autoPrint) => {
    const { frame, print } = setup(autoPrint);
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(400));
    print.mockClear();

    fireEvent.click(frame.contentDocument!.querySelector("button")!);
    expect(print).toHaveBeenCalledTimes(1);
    fireEvent.load(frame);
    act(() => vi.advanceTimersByTime(1000));
    expect(print).toHaveBeenCalledTimes(1);
  });

  it("replaces a queued automatic print when the user prints immediately", () => {
    const { frame, print } = setup();
    fireEvent.load(frame);
    fireEvent.click(frame.contentDocument!.querySelector("button")!);
    act(() => vi.advanceTimersByTime(1000));
    expect(print).toHaveBeenCalledTimes(1);
  });
});
