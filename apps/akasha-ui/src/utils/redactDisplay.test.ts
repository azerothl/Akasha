import { describe, expect, it } from "vitest";

import { redactDisplaySecrets } from "./redactDisplay";

describe("redactDisplaySecrets", () => {
  it("preserves suffix characters around captured secrets", () => {
    expect(redactDisplaySecrets('token="abcdefghijklmnop"')).toBe('token="••••••••"');
  });
});
