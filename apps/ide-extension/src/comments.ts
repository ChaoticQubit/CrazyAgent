import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import { COMMENT_SENT, type IdeChangeSet, commentRefusal, commentTarget, refusal } from "./scm-model.js";

const CONTROLLER = "crazyagent.comments";
const FRESH_MS = 5_000;

/** Lets the owner comment on the agent side of a change set's diff, the file in the agent's
 * worktree: each comment goes to the engine with its file and line and reaches the agent
 * as one message in the project's team conversation. Inputs: the extension context and
 * the engine client. Output: none; all is disposed with the extension. */
export function registerComments(context: vscode.ExtensionContext, engine: Engine): void {
  const controller = vscode.comments.createCommentController(CONTROLLER, "CrazyAgent comments");
  controller.options = { prompt: "Comment for the agent", placeHolder: "What should the agent change here?" };
  let sets: IdeChangeSet[] = [];
  let loaded = 0;
  let loading: Promise<void> | undefined;

  const load = (): Promise<void> => {
    loading ??= engine
      .request<IdeChangeSet[]>("ide.change_sets", {})
      .then(
        (reply) => {
          sets = reply.kind === "ok" && Array.isArray(reply.data) ? reply.data : [];
        },
        () => {
          sets = [];
        },
      )
      .finally(() => {
        loaded = Date.now();
        loading = undefined;
      });
    return loading;
  };

  const fresh = async (): Promise<void> => {
    if (Date.now() - loaded > FRESH_MS) await load();
  };

  controller.commentingRangeProvider = {
    provideCommentingRanges: async (document: vscode.TextDocument): Promise<vscode.Range[]> => {
      if (document.uri.scheme !== "file") return [];
      await fresh();
      if (commentTarget(sets, document.uri.fsPath) === null) return [];
      return [new vscode.Range(0, 0, Math.max(document.lineCount - 1, 0), 0)];
    },
  };

  const send = async (reply: vscode.CommentReply): Promise<void> => {
    const { thread, text } = reply;
    await fresh();
    const target = commentTarget(sets, thread.uri.fsPath);
    if (target === null) {
      void vscode.window.showErrorMessage(refusal("changes_not_found"));
      return;
    }
    const line = (thread.range?.start.line ?? 0) + 1;
    let answer;
    try {
      answer = await engine.request<null>("ide.comment", { id: target.id, path: target.path, line, text });
    } catch {
      void vscode.window.showErrorMessage(refusal("changes_outcome_unknown"));
      return;
    }
    if (answer.kind !== "ok") {
      void vscode.window.showErrorMessage(commentRefusal(answer.code));
      return;
    }
    const body = new vscode.MarkdownString();
    body.appendText(text);
    thread.comments = [...thread.comments, { body, mode: vscode.CommentMode.Preview, author: { name: "You" }, label: "Sent" }];
    thread.canReply = false;
    void vscode.window.showInformationMessage(COMMENT_SENT);
  };

  context.subscriptions.push(
    controller,
    engine.on("change_sets_changed", () => {
      loaded = 0;
    }),
    vscode.commands.registerCommand("crazyagent.comments.send", send),
  );
}
