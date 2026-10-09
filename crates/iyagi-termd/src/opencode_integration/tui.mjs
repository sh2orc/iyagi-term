import reporter from "./reporter.mjs";

export default {
  id: "iyagi-session",
  tui: async api => {
    const report = reporter();
    let current;
    const observe = () => {
      const route = api.route.current;
      if (route.name !== "session") return;
      const id = route.params.sessionID;
      const info = api.state.session.get(id);
      if (!info || info.parentID || current === id) return;
      current = id;
      void report("SessionStart", id, info.directory || api.state.path.directory);
    };
    observe();
    const timer = setInterval(observe, 250);
    timer.unref?.();
    api.lifecycle.onDispose(() => clearInterval(timer));
  },
};
