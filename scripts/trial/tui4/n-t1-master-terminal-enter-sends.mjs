export const checks = [
  {
    id: "N-T1",
    op: ["OP-12"],
    title: "Master view is a live terminal: Shift+Enter adds a line, Enter sends",
    why: "owner: the master should be a real live terminal where Enter sends",
    after: "end",
    run: async (tui) => {
      await tui.go("project-master");
      await tui.waitFor("master");
      const before = tui.pane();
      if (/\bSend\b/.test(before)) {
        throw new Error(`the master view is a chat form, not a terminal:\n${before}`);
      }

      await tui.type("first line");
      await tui.key("Shift-Enter");
      await tui.type("second line");
      const twoLines = tui.pane();
      if (!twoLines.includes("first line") || !twoLines.includes("second line")) {
        throw new Error(`Shift+Enter did not keep both lines in one input:\n${twoLines}`);
      }

      await tui.key("Enter");
      await tui.waitFor("first line");
      const sent = tui.pane();
      if (!sent.includes("first line") || !sent.includes("second line")) {
        throw new Error(`Enter did not send the two lines as one message:\n${sent}`);
      }
      tui.screen("n-t1-two-lines-one-message");
      return "Shift+Enter kept two lines in one input and Enter sent them as one message in the master terminal";
    },
  },
];
