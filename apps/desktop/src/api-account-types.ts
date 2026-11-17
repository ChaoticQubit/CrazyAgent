export type AccountProvider = "chatgpt" | "claude" | "openrouter" | "openai_api" | "anthropic_api" | "openai_compatible" | "local";
export type SignInMethod = "subscription" | "api_key" | "url";
export type AccountField = "hidden" | "optional" | "required";
export type AccountStatus = "signed_in" | "not_signed_in" | "key_saved" | "unreachable" | "signing_in";

export interface SignInMethodEntry {
  method: SignInMethod;
  label: string;
  url: AccountField;
  url_preset: string | null;
  key: AccountField;
  model: AccountField;
}

export interface ProviderEntry {
  provider: AccountProvider;
  name: string;
  methods: SignInMethodEntry[];
}

export interface AccountDraft {
  provider: AccountProvider;
  method: SignInMethod;
  name: string;
  url?: string | null;
  model?: string | null;
}

export interface AccountView {
  id: string;
  name: string;
  provider: AccountProvider;
  provider_name: string;
  method: SignInMethod;
  method_label: string;
  url: string | null;
  model: string | null;
  status: AccountStatus;
  status_label: string;
}

export interface AccountCheck {
  account: AccountView;
  models: string[];
}
