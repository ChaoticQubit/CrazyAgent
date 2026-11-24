/** What a restore of the 2.4 backup says, one sentence per code, the same on every page: the
 * engine kept stopped while it runs, a restore that did not finish or brought 2.4 back, and the
 * reasons a restore was refused. Input: none. Output: the sentence for each code. */
export const RESTORE_COPY: Record<string, string> = {
  engine_restoring: "CrazyAgent is bringing back your 2.4 data, so its engine stays stopped. Wait a minute, then reconnect.",
  engine_restore_broken: "Restore did not finish. Your data is safe in a backup.",
  engine_restored: "Your 2.4 data is back. Install CrazyAgent 2.4.2 to use it. Restart engine goes back to 2.5, which updates that data again.",
  storage_restore_unavailable: "There's no 2.4 backup to bring back. CrazyAgent keeps it for 30 days after the update to 2.5.",
  storage_restore_agents_running: "Agents are running. Stop every agent and research run, then bring back your 2.4 data.",
  storage_restore_busy: "Something else had CrazyAgent's data open, so your 2.4 data wasn't brought back. Close other CrazyAgent windows, then try again.",
  storage_restore_failed: "Your 2.4 data wasn't brought back, and CrazyAgent went on with your 2.5 data. Try again.",
};
