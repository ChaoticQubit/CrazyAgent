import * as vscode from "vscode";
import type { Engine } from "./engine.js";
import {
  HEAD_SCHEME,
  type IdeChangeSet,
  commentRefusal,
  commentSent,
  commentTarget,
  commentTooLong,
  headCommentTarget,
  refusal,
} from "./scm-model.js";

const CONTROLLER = "crazyagent.comments";
const FRESH_MS = 5_000;

/** Lets the owner comment on the agent side of a team agent's change set diff, the text at
 * the head the set recorded, or on the file in the agent's worktree: each comment goes to
 * the engine with its file and line, which delivers it to the agent that made the set. Inputs: the extension context and the engine
 * client. Output: none; all is disposed with the extension. */
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

  const targetOf = (uri: vscode.Uri): { id: string; path: string; agent: string } | null => {
    if (uri.scheme === HEAD_SCHEME) return headCommentTarget(sets, uri.path);
    return uri.scheme === "file" ? commentTarget(sets, uri.fsPath) : null;
  };

  controller.commentingRangeProvider = {
    provideCommentingRanges: async (document: vscode.TextDocument): Promise<vscode.Range[]> => {
      if (document.uri.scheme !== "file" && document.uri.scheme !== HEAD_SCHEME) return [];
      await fresh();
      if (targetOf(document.uri) === null) return [];
      return [new vscode.Range(0, 0, Math.max(document.lineCount - 1, 0), 0)];
    },
  };

  const send = async (reply: vscode.CommentReply): Promise<void> => {
    const { thread, text } = reply;
    await fresh();
    const target = targetOf(thread.uri);
    if (target === null) {
      void vscode.window.showErrorMessage(refusal("changes_not_found"));
      return;
    }
    if (commentTooLong(text)) {
      void vscode.window.showErrorMessage(commentRefusal("ide_comment_too_long"));
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
    void vscode.window.showInformationMessage(commentSent(target.agent));
  };

  context.subscriptions.push(
    controller,
    engine.on("change_sets_changed", () => {
      loaded = 0;
    }),
    vscode.commands.registerCommand("crazyagent.comments.send", send),
  );
}
