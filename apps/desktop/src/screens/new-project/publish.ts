import type { Result } from "../../api";
import type { GitHubOwner, GitHubPublishJob, GitHubPublishPlan } from "../../engine-client";

export const PUBLISH_TOGGLE = "Publish this project on GitHub";
export const PUBLISH_LINE = "Standard secure setup; agents can propose changes later.";
export const PUBLISH_DOCS = "Also publish the docs folder (plans, research)";
export const SIGN_IN_WAIT = "Sign in to GitHub, or turn publishing off.";
export const DETAILS_WAIT = "Fix the GitHub details in Basics first.";
export const PUBLISH_STARTED = "Publishing on GitHub has started.";
export const REPO_NAME_BYTES_MAX = 100;
export const OWNER_BYTES_MAX = 39;
export const DESCRIPTION_CHARS_MAX = 350;

const NAME_CHARACTERS = /^[A-Za-z0-9._-]+$/u;
const OWNER_CHARACTERS = /^[A-Za-z0-9-]+$/u;
const CONTROL = /\p{Cc}/u;

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

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

/** Starts the publish block of a new draft: On, private, the personal account, the name
 * and description following Basics, and the docs folder left out. Input: none. Output:
 * the block. */
export function emptyPublish(): PublishDraft {
  return { on: true, private: true, owner: null, name: null, description: null, docs: false };
}

/** Makes a project name into a GitHub repository name: every character GitHub refuses
 * becomes a dash, dash runs fold to one, leading dots and dashes go, the name is cut to
 * 100 bytes, then trailing dashes and `.git` go. Input: the project name. Output: the
 * repository name, empty when nothing usable is left. */
export function githubName(projectName: string): string {
  let name = projectName.replace(/[^A-Za-z0-9._-]/gu, "-").replace(/-+/gu, "-").replace(/^[.-]+/u, "");
  name = name.slice(0, REPO_NAME_BYTES_MAX);
  for (let previous = ""; previous !== name;) {
    previous = name;
    name = name.replace(/-+$/u, "");
    if (name.endsWith(".git")) name = name.slice(0, -".git".length);
  }
  assert(name === "" || nameValid(name), `githubName made an invalid name: ${name}`);
  return name;
}

/** Makes the project's about text into a repository description: whitespace and control
 * character runs become one space, the text is cut to 350 characters and trimmed.
 * Input: the about text. Output: the description. */
export function githubDescription(about: string): string {
  const folded = about.replace(/[\s\p{Cc}]+/gu, " ").trim();
  const description = [...folded].slice(0, DESCRIPTION_CHARS_MAX).join("").trimEnd();
  assert(descriptionValid(description), "githubDescription made an invalid description");
  return description;
}

function nameValid(name: string): boolean {
  return name.length > 0 && name.length <= REPO_NAME_BYTES_MAX && NAME_CHARACTERS.test(name)
    && !/^[.-]/u.test(name) && !name.endsWith(".git");
}

function ownerValid(owner: string): boolean {
  return owner.length > 0 && owner.length <= OWNER_BYTES_MAX && OWNER_CHARACTERS.test(owner) && !owner.startsWith("-");
}

function descriptionValid(description: string): boolean {
  return [...description].length <= DESCRIPTION_CHARS_MAX && !CONTROL.test(description);
}

/** Fills in the publish block's defaults: the picked account while GitHub still offers it,
 * else the personal one, else the first; the name and description from Basics until the
 * owner typed their own. Inputs: the block, the project name, its about text and the
 * GitHub accounts offered. Output: the values shown. */
export function publishShown(publish: PublishDraft, projectName: string, about: string, owners: GitHubOwner[]): PublishShown {
  const picked = owners.find((owner) => owner.login === publish.owner);
  const owner = picked ?? owners.find((candidate) => candidate.personal) ?? owners[0];
  return {
    owner: owner?.login ?? "",
    name: publish.name ?? githubName(projectName.trim()),
    description: publish.description ?? githubDescription(about),
  };
}

/** Checks the publish block the way the engine checks a publish plan. With publishing off
 * nothing is checked; without a GitHub sign-in only the sign-in is asked for. Inputs: the
 * block, the project name, its about text, the accounts offered and whether CrazyAgent is
 * signed in to GitHub. Output: the problems, empty when the block is valid. */
export function publishErrors(publish: PublishDraft, projectName: string, about: string, owners: GitHubOwner[], signedIn: boolean): PublishErrors {
  if (!publish.on) return {};
  if (!signedIn) return { sign_in: SIGN_IN_WAIT };
  const shown = publishShown(publish, projectName, about, owners);
  const errors: PublishErrors = {};
  if (!shown.owner) errors.owner = "Pick the GitHub account the repository goes under.";
  else if (!ownerValid(shown.owner)) errors.owner = "GitHub sent an account name CrazyAgent can't use. Pick another.";
  if (!shown.name) errors.name = "Enter a repository name.";
  else if (shown.name.length > REPO_NAME_BYTES_MAX) errors.name = `Use at most ${REPO_NAME_BYTES_MAX} characters.`;
  else if (!nameValid(shown.name)) errors.name = "Use letters, digits, dots, dashes and underscores, not starting with a dot or dash or ending in .git.";
  if ([...shown.description].length > DESCRIPTION_CHARS_MAX) errors.description = `Use at most ${DESCRIPTION_CHARS_MAX} characters.`;
  else if (CONTROL.test(shown.description)) errors.description = "Keep the description on one line, without tabs or control characters.";
  return errors;
}

/** Builds the plan a publish Start sends. Inputs: the block, the project name, its about
 * text and the accounts offered. Output: the plan, or null when publishing is off or a
 * field has a problem. */
export function publishPlan(publish: PublishDraft, projectName: string, about: string, owners: GitHubOwner[]): GitHubPublishPlan | null {
  if (!publish.on || Object.keys(publishErrors(publish, projectName, about, owners, true)).length > 0) return null;
  const shown = publishShown(publish, projectName, about, owners);
  assert(ownerValid(shown.owner) && nameValid(shown.name) && descriptionValid(shown.description), "publishPlan built an invalid plan");
  return { owner: shown.owner, name: shown.name, description: shown.description, private: publish.private, docs: publish.docs };
}

/** Says why Create project waits on the publish block: no GitHub sign-in, or a field the
 * engine would refuse. Inputs: the block, the project name, its about text, the accounts
 * offered and whether CrazyAgent is signed in to GitHub. Output: the sentence, or null
 * while publishing is off or the block is valid. */
export function createWait(publish: PublishDraft, projectName: string, about: string, owners: GitHubOwner[], signedIn: boolean): string | null {
  return null;
}

/** Starts publishing a made project on GitHub with the block's plan. Inputs: the project
 * id, the block, the project name, its about text and the accounts offered. Output: the
 * start's result, or null when publishing is off or the block is not valid. */
export function startPublish(projectId: string, publish: PublishDraft, projectName: string, about: string, owners: GitHubOwner[]): Promise<Result<GitHubPublishJob>> | null {
  return null;
}
