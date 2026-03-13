import { describe, it, expect } from "vitest";
import { preprocessDataUrlImages } from "./preprocessDataUrlImages";

describe("preprocessDataUrlImages", () => {
  it("converts markdown data URL image to div wrap and img", () => {
    const input = "![Photo](<data:image/jpeg;base64,ABC>)";
    const out = preprocessDataUrlImages(input);
    expect(out).toContain('<div class="markdown-data-image-wrap">');
    expect(out).toContain('<img src="data:image/jpeg;base64,ABC"');
    expect(out).toContain('alt="Photo"');
    expect(out).toContain('class="markdown-data-image"');
  });

  it("escapes alt in HTML attribute", () => {
    const input = '![A"b&c<](<data:image/png;base64,XYZ>)';
    const out = preprocessDataUrlImages(input);
    expect(out).toContain('alt="A&quot;b&amp;c&lt;"');
    expect(out).toContain('src="data:image/png;base64,XYZ"');
  });

  it("returns empty string unchanged", () => {
    expect(preprocessDataUrlImages("")).toBe("");
  });

  it("returns string with no data image match unchanged", () => {
    const input = "Hello ![normal](https://example.com/img.png) world";
    const out = preprocessDataUrlImages(input);
    expect(out).toBe(input);
  });

  it("processes multiple data URL images", () => {
    const input =
      "![First](<data:image/jpeg;base64,A>) text ![Second](<data:image/png;base64,B>)";
    const out = preprocessDataUrlImages(input);
    expect(out).toContain('<div class="markdown-data-image-wrap">');
    expect(out).toContain('src="data:image/jpeg;base64,A"');
    expect(out).toContain('src="data:image/png;base64,B"');
    expect(out).toContain("text");
  });

  it("uses default alt when no ![ before marker", () => {
    const input = "](<data:image/jpeg;base64,X>)";
    const out = preprocessDataUrlImages(input);
    expect(out).toContain('alt="Image"');
    expect(out).toContain('src="data:image/jpeg;base64,X"');
  });
});
