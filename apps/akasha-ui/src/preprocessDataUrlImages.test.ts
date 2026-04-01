import { describe, it, expect } from "vitest";
import { preprocessDataUrlImages, preprocessDataUrlAudio } from "./preprocessDataUrlImages";
import { preprocessMessagePaths } from "./preprocessMessagePaths";

describe("preprocessDataUrlImages", () => {
  it("converts markdown data URL image to div wrap and img", () => {
    const input = "![Photo](<data:image/jpeg;base64,ABC>)";
    const out = preprocessDataUrlImages(input);
    expect(out).toContain('<div class="markdown-data-image-wrap">');
    expect(out).toContain('<img src="data:image/jpeg;base64,ABC"');
    expect(out).toContain('alt="Photo"');
    expect(out).toContain('class="markdown-data-image"');
  });

  it("converts plain-parens markdown data URL image (no angle brackets)", () => {
    const input = "![Photo capturée](data:image/jpeg;base64,/9j/TEST)";
    const out = preprocessDataUrlImages(input);
    expect(out).toContain('<div class="markdown-data-image-wrap">');
    expect(out).toContain('<img src="data:image/jpeg;base64,/9j/TEST"');
    expect(out).toContain('alt="Photo capturée"');
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

  it("combined with preprocessMessagePaths does not turn </div> into path link (photo reply)", () => {
    const message = "Photo captured. It is shown below.\n\n![Photo](<data:image/jpeg;base64,ABC>)";
    const withDiv = preprocessDataUrlImages(message);
    const withPaths = preprocessMessagePaths(withDiv);
    expect(withPaths).toContain('<div class="markdown-data-image-wrap">');
    expect(withPaths).toContain("</div>");
    expect(withPaths).not.toMatch(/\]\(path:[^)]*%2F%3E%3C%2Fdiv/);
  });

  it("skips (does not inject) a data URL that fails strict validation", () => {
    // URL contains a quote character — should not be injected as HTML
    const input = '![img](<data:image/jpeg;base64,">)';
    const out = preprocessDataUrlImages(input);
    expect(out).not.toContain("<img");
    expect(out).not.toContain("<div");
  });

  it("escapes src URL for HTML attribute", () => {
    // Even a valid base64 URL should be attr-escaped in src=""
    const input = "![x](<data:image/png;base64,ABC>)";
    const out = preprocessDataUrlImages(input);
    // Basic check: src attribute contains expected data URL
    expect(out).toContain('src="data:image/png;base64,ABC"');
  });
});

describe("preprocessDataUrlAudio", () => {
  it("converts markdown data URL audio to div wrap and audio controls", () => {
    const input = "![Audio synthétisé](<data:audio/wav;base64,ABC>)";
    const out = preprocessDataUrlAudio(input);
    expect(out).toContain('<div class="markdown-data-audio-wrap">');
    expect(out).toContain('<audio controls src="data:audio/wav;base64,ABC"');
    expect(out).toContain('class="markdown-data-audio"');
  });

  it("returns string with no data audio match unchanged", () => {
    const input = "Hello ![img](<data:image/png;base64,X>) world";
    const out = preprocessDataUrlAudio(input);
    expect(out).toBe(input);
  });
});

describe("preprocessMessagePaths", () => {
  it("does not convert </tag> closing HTML tags into path links", () => {
    const input = "text </div> more";
    const out = preprocessMessagePaths(input);
    expect(out).toContain("</div>");
    expect(out).not.toContain("path:");
  });

  it("does not convert self-closing /> into path links", () => {
    const input = '<br /> and <img src="x" />';
    const out = preprocessMessagePaths(input);
    expect(out).not.toContain("path:");
  });

  it("converts a Unix path preceded by whitespace", () => {
    const input = "see /home/user/file.txt for details";
    const out = preprocessMessagePaths(input);
    expect(out).toContain("path:");
    expect(out).toContain("/home/user/file.txt");
  });
});
