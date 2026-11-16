import { APPROVAL_PAGE, type ApprovalView } from "../../api-connectors";
import type { Loaded } from "../../state";
import { PageHeader } from "../../ui";

/** What the approval page asks its container to do. */
export interface ApprovalActions {
  onAnswer: (allow: boolean) => void;
  onRetry: () => void;
}

/** The approval page for one connector write. Inputs: the approval as read, now in epoch
 * milliseconds, a busy flag, the last failure and the actions. Output: the page. */
export function ApprovalPageView({ view, now, busy, failure, actions }: {
  view: Loaded<ApprovalView>;
  now: number;
  busy: boolean;
  failure: string;
  actions: ApprovalActions;
}) {
  void view;
  void now;
  void busy;
  void failure;
  void actions;
  return <PageHeader title={APPROVAL_PAGE.title} />;
}

/** The approval page the ConnectorWrite notice opens. Input: the approval id from the route.
 * Output: the page. */
export function ConnectorApprovalPage({ id }: { id: number }) {
  void id;
  return <ApprovalPageView view={{ kind: "loading" }} now={0} busy={false} failure="" actions={{ onAnswer: () => {}, onRetry: () => {} }} />;
}
