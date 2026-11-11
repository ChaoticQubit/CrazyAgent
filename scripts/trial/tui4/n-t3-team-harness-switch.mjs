export const checks = [
  {
    id: "N-T3",
    op: ["OP-28"],
    title: "Switch the master harness on Team settings and back keeps one conversation with a divider",
    why: "owner: one input; the conversation carries over with the switch divider",
    after: "end",
    run: async (tui) => {
      await tui.go("project-team");
      await tui.waitFor("master");
      const first = tui.pane();
      const firstHarness = /harness[:\s]+([A-Za-z0-9 _-]+)/i.exec(first)?.[1]?.trim();

      await tui.key("h");
      await tui.waitFor("master");
      const switched = tui.pane();
      const nextHarness = /harness[:\s]+([A-Za-z0-9 _-]+)/i.exec(switched)?.[1]?.trim();
      if (firstHarness && nextHarness && firstHarness === nextHarness) {
        throw new Error(`the harness did not switch: still ${firstHarness}`);
      }
      if (!/divider|switched to|now on/i.test(switched)) {
        throw new Error(`no switch divider after the harness switch:\n${switched}`);
      }
      tui.screen("n-t3-switch-divider");

      await tui.key("h");
      await tui.waitFor("master");
      const back = tui.pane();
      const dividers = (back.match(/divider|switched to|now on/gi) || []).length;
      if (dividers < 2) {
        throw new Error(`switching back did not add a second divider to the one conversation:\n${back}`);
      }
      return "the master harness switched and switched back in one conversation, each switch marked by a divider";
    },
  },
];
