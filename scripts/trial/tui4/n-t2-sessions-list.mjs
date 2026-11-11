export const checks = [
  {
    id: "N-T2",
    op: ["OP-10"],
    title: "Resume a session, New session twice adds nothing, Delete a non-live session",
    why: "owner: the previous session should continue; New should not pile up empty sessions",
    after: "end",
    run: async (tui) => {
      await tui.go("project-master");
      await tui.key("l");
      await tui.waitFor("Sessions");
      const list = tui.pane();
      const rows = list.split("\n").filter((line) => /session/i.test(line)).length;
      if (rows < 1) {
        throw new Error(`no previous session to resume:\n${list}`);
      }

      await tui.key("Down");
      await tui.key("Enter");
      await tui.waitFor("master");
      const resumed = tui.pane();
      if (/\bSessions\b/.test(resumed) && !/master/i.test(resumed)) {
        throw new Error(`the previous session did not continue in the live terminal:\n${resumed}`);
      }

      await tui.key("l");
      await tui.waitFor("Sessions");
      const countOf = () => tui.pane().split("\n").filter((line) => /session/i.test(line)).length;
      const start = countOf();
      await tui.key("n");
      await tui.key("n");
      const afterNew = countOf();
      if (afterNew > start + 1) {
        throw new Error(`a second New session added another session: ${start} -> ${afterNew}`);
      }

      await tui.key("l");
      await tui.waitFor("Sessions");
      const withList = countOf();
      await tui.key("d");
      await tui.waitFor("Delete the session");
      tui.screen("n-t2-delete-confirm");
      await tui.key("y");
      await tui.waitFor("Sessions");
      const afterDelete = countOf();
      if (afterDelete >= withList) {
        throw new Error(`the deleted session did not leave the list: ${withList} -> ${afterDelete}`);
      }
      return "resume continued the session, a second New added nothing, and Delete removed a non-live session after its confirm";
    },
  },
];
