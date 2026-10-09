import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { AgentIcon } from "./AgentIcon";

describe("AgentIcon", () => {
  it.each([
    ["claude", "Claude"],
    ["codex", "OpenAI"],
  ])("inlines the bundled %s mark as SVG markup", (agent, title) => {
    const html = renderToStaticMarkup(<AgentIcon agent={agent} />);
    // Inline markup is coloured by `.agent-icon svg { fill: currentColor }`. A CSS mask instead
    // needs WebView2 mask support and a correctly quoted production data URI, and blanks the
    // icon when either is missing.
    expect(html).toMatch(/^<span class="agent-icon" aria-hidden="true"><svg[\s>]/);
    expect(html).toContain(`<title>${title}</title>`);
    expect(html).not.toContain("mask-image");
    expect(html).not.toContain("style=");
  });

  it("renders nothing for an agent without a mark", () => {
    expect(renderToStaticMarkup(<AgentIcon agent="unknown" />)).toBe("");
  });
});
