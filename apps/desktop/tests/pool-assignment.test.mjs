import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      for (const extension of [".ts", ".tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) return nextLoad(url, context);
    const transformed = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022",
      sourceType: "module",
      jsx: { runtime: "automatic" },
    });
    assert.deepEqual(transformed.errors, [], "production source must load before behavior assertions");
    return { format: "module", source: transformed.code, shortCircuit: true };
  },
});

const { ModelAccess } = await import("../src/screens/model-access.tsx");
const { useOwnerBrowser } = await import("../src/state.ts");
const { formFromRecord, editFromForm } = await import("../src/model-access.ts");

const catalogPool = {
  id: "catalog-pool", members: ["saved-connection"], projects: [], roles: [],
  fallback_allowed: true, provider_quota_policy: "strict",
};

function RetainedPool({ pool, projectRows = [] }) {
  const state = useOwnerBrowser();
  return createElement(ModelAccess, { state: {
    ...state,
    modelCollection: "pools",
    modelForm: { value: formFromRecord({ kind: "pool", pool }, "retained-draft"), generation: 1 },
    projectPage: { items: projectRows, has_more: false, fresh: false },
  } });
}

test("the actual retained pool form labels blank scopes Unassigned without wildcard guidance", () => {
  const markup = renderToStaticMarkup(createElement(RetainedPool, { pool: catalogPool }));
  const form = markup.match(/<form\b[^>]*data-testid="model-access-form"[^>]*>[\s\S]*?<\/form>/)?.[0];
  assert.ok(form, "the real retained metadata form must render before assignment assertions");
  assert.match(form, /Unassigned/, "a saved catalog pool with empty scopes must visibly remain Unassigned");
  assert.doesNotMatch(form, /available without a scope restriction|unrestricted|all projects|all roles/i);
  assert.match(form, /Create or open a project/);
  const save = form.match(/<button\b[^>]*data-testid="save-model-access"[^>]*>/)?.[0];
  assert.ok(save);
  assert.match(save, /disabled=""/, "a disconnected retained form cannot publish settings");
  assert.deepEqual(catalogPool.projects, []);
  assert.deepEqual(catalogPool.roles, []);
});

test("the actual pool form offers explicit project identity choices and a separate master-role confirmation", () => {
  const projectRows = [
    { id: "project-first", name: "Same project name", repository_count: 2 },
    { id: "project-second", name: "Same project name", repository_count: 2 },
  ];
  const markup = renderToStaticMarkup(createElement(RetainedPool, { pool: catalogPool, projectRows }));
  const select = markup.match(/<select\b[^>]*data-testid="pool-assignment-project"[^>]*>[\s\S]*?<\/select>/)?.[0];
  assert.ok(select, "the ordinary pool form must offer project selection instead of requiring raw project IDs");
  assert.match(select, /disabled=""/, "stale display input is never an authorized binding basis");
  assert.match(select, /<option\b[^>]*value=""[^>]*selected=""[^>]*>Choose a project<\/option>/);
  assert.match(select, /<option\b[^>]*value="project-first"[^>]*>/);
  assert.match(select, /<option\b[^>]*value="project-second"[^>]*>/);
  assert.match(markup, /project_leader/);
  const confirm = markup.match(/<button\b[^>]*data-testid="confirm-pool-project"[^>]*>/)?.[0];
  assert.ok(confirm, "project and project_leader assignment requires an explicit confirmation");
  assert.match(confirm, /disabled=""/);
  assert.deepEqual(catalogPool.projects, [], "rendering project choices must never assign the catalog pool");
  assert.deepEqual(catalogPool.roles, []);
  assert.ok(Buffer.byteLength(markup, "utf8") < 65536);
});

test("partially assigned retained forms remain Unassigned and preserve their exact configuration", () => {
  for (const pool of [
    { ...catalogPool, projects: ["project-first"] },
    { ...catalogPool, roles: ["builder"] },
  ]) {
    const before = structuredClone(pool);
    const markup = renderToStaticMarkup(createElement(RetainedPool, { pool }));
    assert.match(markup, /Unassigned/, "both real project and role assignments are required before a pool can be used");
    assert.deepEqual(editFromForm(formFromRecord({ kind: "pool", pool }, "retained-draft")), {
      kind: "replace_pool", pool: before,
    });
    assert.deepEqual(pool, before);
    assert.doesNotMatch(markup, /(?:Account authenticated|Model ready|Master running)/);
  }
});
