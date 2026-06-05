// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { InfoTip } from "../components/Tooltip";

describe("InfoTip", () => {
  it("renders trigger with aria help role", () => {
    render(<InfoTip label="Help" content="Tooltip body" />);
    expect(screen.getByRole("button", { name: "?" })).toBeTruthy();
    expect(screen.getByText("Help", { selector: ".sr-only" })).toBeTruthy();
  });
});
