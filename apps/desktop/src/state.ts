import { useEffect, useRef, useState } from "react";
import {
  approveBaseline, baselines, connect, errorMessage, inspectBaseline, reconcile,
  retry, revokeBaseline, status,
} from "./engine-client";
import type { ActionReceipt, BaselineDetail, BaselinePage } from "./engine-client";

function receiptMessage(receipt: ActionReceipt): string {
  switch (receipt.outcome) {
    case "applied": return "The engine recorded the decision.";
    case "rejected": return "The engine rejected this decision. No successful effect is claimed.";
    case "unavailable": return "The engine recorded execution as unavailable.";
    case "not_recorded_at_barrier": return "The intent was not recorded at the observed barrier. Delivery may still arrive; it remains pending.";
    case "unknown": return "The engine reports an unknown outcome. The exact intent remains pending.";
    case "uncertain": return "Delivery is uncertain. Reconnect and reconcile the retained exact intent.";
  }
}

export function useOwnerBrowser() {
  const [page, setPage] = useState<BaselinePage>();
  const [detail, setDetail] = useState<BaselineDetail>();
  const [connected, setConnected] = useState(false);
  const [pending, setPending] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const running = useRef(false);

  async function run(action: () => Promise<void>) {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await action();
    } catch (failure: unknown) {
      setError(errorMessage(failure));
      setPage((old) => old ? { ...old, fresh: false } : old);
      setDetail((old) => old ? { ...old, fresh: false } : old);
    } finally {
      try {
        const native = await status();
        setPending(native.pending);
        setConnected(native.connected);
      } catch {
        setConnected(false);
        setPage((old) => old ? { ...old, fresh: false } : old);
        setDetail((old) => old ? { ...old, fresh: false } : old);
      }
      running.current = false;
      setBusy(false);
    }
  }

  async function reconnect() {
    setConnected(false);
    setDetail(undefined);
    const result = await connect();
    setConnected(result.connected);
    setPending(result.pending);
    setPage(await baselines());
  }

  async function refresh(next = false) {
    setDetail(undefined);
    setPage(await baselines(next));
  }

  async function finish(receipt: ActionReceipt) {
    setPending(receipt.pending);
    setNotice(receiptMessage(receipt));
    setPage((old) => old ? { ...old, fresh: false } : old);
    setDetail((old) => old ? { ...old, fresh: false } : old);
    if (!receipt.pending) {
      setPage(await baselines());
      setDetail(await inspectBaseline(receipt.baseline_id));
    }
  }

  useEffect(() => { void run(reconnect); }, []);

  return {
    page, detail, connected, pending, busy, error, notice,
    reconnect: () => run(reconnect),
    refresh: (next = false) => run(() => refresh(next)),
    select: (id: string) => run(async () => setDetail(await inspectBaseline(id))),
    approve: () => run(async () => {
      if (detail) await finish(await approveBaseline(detail.ticket));
    }),
    revoke: () => run(async () => {
      if (detail) await finish(await revokeBaseline(detail.ticket));
    }),
    reconcile: () => run(async () => finish(await reconcile())),
    retry: () => run(async () => finish(await retry())),
  };
}
