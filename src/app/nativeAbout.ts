import type { AboutMetadata } from "@tauri-apps/api/menu";
import { translate, type Language } from "../i18n";

/** Native About panels use credits on macOS and comments on Windows/Linux. */
export function nativeAboutMetadata(language: Language, version: string): AboutMetadata {
  const t = (key: string) => translate(language, key);
  const introduction = [
    t("about.summary"),
    t("about.cli"),
    [t("about.workspace"), t("about.resources"), t("about.sessions")].join("\n"),
  ].join("\n\n");
  return {
    name: t("app.name"),
    version,
    authors: [t("app.name")],
    license: "Proprietary",
    comments: introduction,
    credits: [
      introduction,
      `${t("about.builtWith")}\nTauri · React · xterm.js · Rust`,
      `${t("about.developedBy")} ${t("app.name")}\n${t("about.license")}: Proprietary`,
    ].join("\n\n"),
  };
}
