// Strings of this fork's features (meeting mode), kept in
// locales/*/fork.json so the upstream translation.json files stay untouched
// and syncing with upstream doesn't conflict on them.
type Tree = Record<string, unknown>;

const forkModules = import.meta.glob<{ default: Tree }>(
  "./locales/*/fork.json",
  { eager: true },
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

/** The locale's upstream strings plus this fork's ones. */
export const withForkStrings = (langCode: string, translation: Tree): Tree => {
  const fork = forkModules[`./locales/${langCode}/fork.json`]?.default;
  return fork ? deepMerge(translation, fork) : translation;
};
