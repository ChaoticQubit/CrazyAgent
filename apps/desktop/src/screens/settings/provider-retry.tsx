import type { ProviderRetry, ProviderRetryView } from "../../api";
import type { Loaded } from "../../state";

/** The setting's options in list order, each with its title and what it does. */
export const PROVIDER_RETRY_CHOICES: { choice: ProviderRetry; title: string; summary: string }[] = [];

/** The option a fresh engine runs under, the engine's default `ProviderRetry`. */
export const PROVIDER_RETRY_DEFAULT: ProviderRetry = "harness_default";

/** The setting's section. Inputs: the read, the picked option, the save state and actions.
 * Output: the section, or nothing while the engine has no such setting. */
export function ProviderRetrySettingsView(_props: {
  retry: Loaded<ProviderRetryView>; picked: ProviderRetry | null; saving: boolean; problem: string;
  onPick: (choice: ProviderRetry) => void; onSave: () => void; onRetry: () => void;
}) {
  return null;
}
