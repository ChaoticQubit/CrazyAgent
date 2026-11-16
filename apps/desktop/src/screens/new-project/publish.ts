import type { GitHubOwner, GitHubPublishPlan } from "../../engine-client";

export const PUBLISH_TOGGLE = "";
export const PUBLISH_LINE = "";
export const PUBLISH_DOCS = "";
export const SIGN_IN_WAIT = "";
export const REPO_NAME_BYTES_MAX = 100;
export const OWNER_BYTES_MAX = 39;
export const DESCRIPTION_CHARS_MAX = 350;

/** The publish block of a New Project draft: whether the project goes to GitHub, its
 * visibility, and the owner, name and description the owner typed, each null while it
 * follows its default, and whether the docs folder goes up too. */
export interface PublishDraft {
  on: boolean;
  private: boolean;
  owner: string | null;
  name: string | null;
  description: string | null;
  docs: boolean;
}

/** The values the publish block shows once its defaults are filled in. */
export interface PublishShown {
  owner: string;
  name: string;
  description: string;
}

/** One plain message per publish field with a problem. */
export interface PublishErrors {
  sign_in?: string;
  owner?: string;
  name?: string;
  description?: string;
}

/** Starts the publish block of a new draft. Input: none. Output: the block. */
export function emptyPublish(): PublishDraft {
  return { on: false, private: false, owner: null, name: null, description: null, docs: false };
}

/** Makes a project name into a GitHub repository name. Input: the project name. Output:
 * the repository name. */
export function githubName(_projectName: string): string {
  return "";
}

/** Makes the project's about text into a repository description. Input: the about text.
 * Output: the description. */
export function githubDescription(_about: string): string {
  return "";
}

/** Fills in the publish block's defaults. Inputs: the block, the project name, its about
 * text and the GitHub accounts offered. Output: the values shown. */
export function publishShown(_publish: PublishDraft, _projectName: string, _about: string, _owners: GitHubOwner[]): PublishShown {
  return { owner: "", name: "", description: "" };
}

/** Checks the publish block. Inputs: the block, the project name, its about text, the
 * accounts offered and whether CrazyAgent is signed in to GitHub. Output: the problems. */
export function publishErrors(_publish: PublishDraft, _projectName: string, _about: string, _owners: GitHubOwner[], _signedIn: boolean): PublishErrors {
  return {};
}

/** Builds the plan a publish Start sends. Inputs: the block, the project name, its about
 * text and the accounts offered. Output: the plan, or null when publishing is off or a
 * field has a problem. */
export function publishPlan(_publish: PublishDraft, _projectName: string, _about: string, _owners: GitHubOwner[]): GitHubPublishPlan | null {
  return null;
}
