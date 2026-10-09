import claude from "../../assets/agents/claude.svg?raw";
import openai from "../../assets/agents/openai.svg?raw";
import opencodeDark from "../../assets/agents/opencode-dark.svg";
import opencodeLight from "../../assets/agents/opencode-light.svg";

/** Decorative service mark; the adjacent title supplies the accessible name. */
export function AgentIcon({ agent }: { agent: string }): JSX.Element | null {
  if (agent === "opencode") return (
    <span className="agent-icon" aria-hidden="true">
      <img className="agent-icon-dark" src={opencodeDark} alt="" />
      <img className="agent-icon-light" src={opencodeLight} alt="" />
    </span>
  );
  const source = agent === "claude" ? claude : agent === "codex" ? openai : null;
  if (!source) return null;
  // Inline the trusted, bundled marks so Windows WebView2 does not need a CSS
  // image mask. Only these local SVG assets may be used as markup here.
  return <span className="agent-icon" aria-hidden="true" dangerouslySetInnerHTML={{ __html: source }} />;
}
