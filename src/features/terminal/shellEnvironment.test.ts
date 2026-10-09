import { execFileSync } from "node:child_process";
import { describe, expect, it } from "vitest";
import { cleanShellCommand, terminalDisplayTitle } from "./shellEnvironment";

describe("shell color environment", () => {
  it("shows the actual shell name for existing launcher titles", () => {
    expect(terminalDisplayTitle("env -u NO_COLOR -u FORCE_COLOR -u CLICOLOR -u CLICOLOR_FORCE /bin/zsh -l -i")).toBe("zsh");
    expect(terminalDisplayTitle("/usr/bin/env -u NO_COLOR -u FORCE_COLOR -u CLICOLOR -u CLICOLOR_FORCE /bin/bash -i")).toBe("bash");
    expect(terminalDisplayTitle("My project terminal")).toBe("My project terminal");
    expect(terminalDisplayTitle("env node server.js")).toBe("env node server.js");
  });
  it.skipIf(process.platform === "win32")("removes conflicting colors even when an older daemon passes them on", () => {
    const command = cleanShellCommand("darwin", { program: process.execPath, argv: ["-e", "console.log(JSON.stringify([process.env.NO_COLOR,process.env.FORCE_COLOR,process.env.CLICOLOR,process.env.CLICOLOR_FORCE]))"] });
    const output = execFileSync(command.program, command.argv, {
      env: { ...process.env, NO_COLOR: "1", FORCE_COLOR: "3", CLICOLOR: "0", CLICOLOR_FORCE: "1" }, encoding: "utf8",
    });
    expect(JSON.parse(output)).toEqual([null, null, null, null]);
  });
  it("preserves the selected Windows shell and arguments", () => {
    const shell = { program: "C:\\PowerShell\\pwsh.exe", argv: ["-NoLogo"] };
    expect(cleanShellCommand("windows", shell)).toEqual(shell);
  });
});
