// This fork's strings live in locales/*/fork.json (see src/i18n/fork.ts), so
// each locale is checked with them merged in, as the app loads it.
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

type Tree = Record<string, unknown>;

const LOCALES_DIR = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "src",
  "i18n",
  "locales",
);

const isTree = (value: unknown): value is Tree =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const deepMerge = (base: Tree, extra: Tree): Tree => {
  const out: Tree = { ...base };
  for (const [key, value] of Object.entries(extra)) {
    out[key] =
      isTree(value) && isTree(out[key])
        ? deepMerge(out[key] as Tree, value)
        : value;
  }
  return out;
};

export function withForkFile(lang: string, translation: Tree): Tree {
  const forkPath = path.join(LOCALES_DIR, lang, "fork.json");
  if (!fs.existsSync(forkPath)) return translation;
  return deepMerge(translation, JSON.parse(fs.readFileSync(forkPath, "utf8")));
}
