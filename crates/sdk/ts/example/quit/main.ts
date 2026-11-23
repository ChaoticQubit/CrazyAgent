import type { Host, Result, Unsubscribe } from "@crazyagent/sdk/core";
import { subscribe } from "@crazyagent/sdk/core";
import { settings } from "@crazyagent/sdk/generated";

/** The first quit event the sample sees, with its seq. */
export type QuitSeen = { readonly seq: bigint; readonly event: settings.SettingsEvent };

/** Runs the quit sample: saves the quit settings through the bus, then subscribes to
 * `AgentQuitSaved` and stops at the first event. Input: the host the kernel bound to this
 * plugin. Output: the first event with its seq, the save's refusal, or the subscription's
 * failure. */
export async function run(host: Host): Promise<Result<QuitSeen>> {
  const saved = await settings.saveQuit(host, {
    on_quit: "stop",
    left_running: "never",
    hours: 12,
  });
  if (!saved.ok) {
    return saved;
  }
  return new Promise((resolve) => {
    let done = false;
    let stop: Unsubscribe | null = null;
    stop = subscribe(
      host,
      settings.SETTINGS_EVENT_KINDS,
      settings.decodeSettingsEvent,
      null,
      (event) => {
        if (done) {
          return;
        }
        done = true;
        stop?.();
        resolve(event);
      },
    );
    if (done) {
      stop();
    }
  });
}
