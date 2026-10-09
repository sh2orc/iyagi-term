import { useI18n } from "../i18n";
import { nativeAboutMetadata } from "./nativeAbout";

export function AboutDialog({ version, onClose }: { version: string; onClose: () => void }): JSX.Element {
  const { t, language } = useI18n();
  const metadata = nativeAboutMetadata(language, version);
  return (
    <div className="about-dialog" onKeyDown={(event) => {
      if (event.key === "Escape") onClose();
      // The close button is the only interactive control in this dialog.
      if (event.key === "Tab") event.preventDefault();
    }}>
      <header className="about-header">
        <div>
          <h2 id="about-title">{metadata.name}</h2>
          <p>{t("about.summary")}</p>
        </div>
        <span className="about-version">v{metadata.version}</span>
      </header>
      <p className="about-introduction">{t("about.cli")}</p>
      <ul className="about-features">
        {["about.workspace", "about.resources", "about.sessions"].map((key) => <li key={key}>{t(key).replace(/^•\s*/, "")}</li>)}
      </ul>
      <dl className="about-details">
        <div><dt>{t("about.builtWith")}</dt><dd>Tauri · React · xterm.js · Rust</dd></div>
        <div><dt>{t("about.developedBy").replace(/:$/, "")}</dt><dd>{metadata.authors?.join(", ")}</dd></div>
        <div><dt>{t("about.license")}</dt><dd>{metadata.license}</dd></div>
      </dl>
      <footer className="about-footer">
        <button type="button" className="quit-cancel" autoFocus onClick={onClose}>{t("notice.confirm")}</button>
      </footer>
    </div>
  );
}
