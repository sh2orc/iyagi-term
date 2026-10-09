import reporter from "./reporter.mjs";

// Legacy server-plugin API, also supported by OpenCode 1.18.x.
export default async function IyagiSession({ client, directory }) {
  const report = reporter();
  let current;
  let pending = Promise.resolve();
  const serial = task => {
    pending = pending.then(task).catch(() => {});
    return pending;
  };
  const select = async info => {
    if (!info || info.parentID || !info.id) return;
    const changed = current !== info.id;
    current = info.id;
    await report(changed ? "SessionStart" : "UserPromptSubmit", current, info.directory || directory);
  };
  return {
    event: ({ event }) => serial(async () => {
      if (event.type === "session.created") await select(event.properties?.info);
    }),
    "chat.message": input => serial(async () => {
      // Metadata lookup distinguishes the main conversation from agent-created
      // children, whose events must never replace the terminal's resume ID.
      const result = await client.session.get({ path: { id: input.sessionID } });
      await select(result.data);
    }),
  };
}
