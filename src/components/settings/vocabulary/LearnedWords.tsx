// Fork feature: custom words learned per model from the dictation history
// (see src-tauri/src/vocabulary). Shown under the user's own custom words.
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  type LearnedVocabulary,
  type LearnedWord,
  type Suspect,
} from "@/bindings";
import { notify } from "@/stores/noticeStore";
import { useModelStore } from "@/stores/modelStore";
import { Button } from "../../ui/Button";
import { Dropdown } from "../../ui/Dropdown";
import { Input } from "../../ui/Input";
import { SettingContainer } from "../../ui/SettingContainer";
import { Spinner } from "../../ui/Spinner";

/** `PROGRESS_EVENT` in vocabulary/learn.rs. */
const PROGRESS_EVENT = "vocabulary-learning-progress";
const AUTO_REFERENCE = "__auto__";
/** Same limit as the custom words field and `MAX_WORD_CHARS`. */
const MAX_WORD_CHARS = 50;

interface LearnProgress {
  phase: "transcribing" | "comparing";
  done: number;
  total: number;
}

type CommandResult =
  | { status: "ok"; data: LearnedVocabulary }
  | { status: "error"; error: string };

interface LearnedWordsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const removeIcon = (
  <svg
    className="w-3 h-3"
    fill="none"
    stroke="currentColor"
    viewBox="0 0 24 24"
    aria-hidden="true"
  >
    <path
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth={2}
      d="M6 18L18 6M6 6l12 12"
    />
  </svg>
);

/** Strongest first: most misheard, then most recent. */
const byStrength = <T extends { count: number; last_seen?: number }>(
  a: T,
  b: T,
) => b.count - a.count || (b.last_seen ?? 0) - (a.last_seen ?? 0);

export const LearnedWords: React.FC<LearnedWordsProps> = ({
  descriptionMode = "tooltip",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const models = useModelStore((state) => state.models);
  const currentModel = useModelStore((state) => state.currentModel);
  const [vocabulary, setVocabulary] = useState<LearnedVocabulary | null>(null);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<LearnProgress | null>(null);
  /** What the last run did, shown under the button. */
  const [summary, setSummary] = useState<string | null>(null);
  /** The learned word being respelled, and its draft. */
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  /** The correct word typed for each suspect, by `heard_as`. */
  const [answers, setAnswers] = useState<Record<string, string>>({});

  useEffect(() => {
    commands.getLearnedVocabulary().then(setVocabulary);
    const unlisten = listen<LearnProgress>(PROGRESS_EVENT, (event) =>
      setProgress(event.payload),
    );
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  const modelName = useCallback(
    (id: string) => models.find((m) => m.id === id)?.name ?? id,
    [models],
  );

  const referenceOptions = useMemo(
    () => [
      {
        value: AUTO_REFERENCE,
        label: t("settings.advanced.learnedWords.auto"),
      },
      ...models
        .filter(
          (m) =>
            m.is_downloaded && m.accuracy_score > 0 && m.id !== currentModel,
        )
        .sort((a, b) => b.accuracy_score - a.accuracy_score)
        .map((m) => ({ value: m.id, label: m.name })),
    ],
    [models, currentModel, t],
  );

  const modelVocabulary = vocabulary?.models?.[currentModel];
  const learnedWords: LearnedWord[] = useMemo(
    () => [...(modelVocabulary?.words ?? [])].sort(byStrength),
    [modelVocabulary],
  );
  const suspects: Suspect[] = useMemo(
    () =>
      (modelVocabulary?.suspects ?? [])
        .filter((s) => !s.dismissed)
        .sort(byStrength),
    [modelVocabulary],
  );

  const applyResult = (result: CommandResult) => {
    if (result.status === "ok") {
      setVocabulary(result.data);
    } else {
      notify({ title: result.error, level: "error" });
    }
  };

  const handleLearn = async () => {
    setRunning(true);
    setProgress(null);
    setSummary(null);
    try {
      const result = await commands.learnCustomWords();
      if (result.status === "error") {
        notify({
          title: t("settings.advanced.learnedWords.failed"),
          detail: result.error,
          level: "error",
        });
        return;
      }
      const report = result.data;
      const all = (perModel: Partial<Record<string, string[]>>) =>
        Object.values(perModel).flatMap((words) => words ?? []);
      const learned = all(report.learned);
      const reinforced = all(report.reinforced);
      const suspected = all(report.suspects);
      if (report.without_reference.length > 0) {
        notify({
          title: t("settings.advanced.learnedWords.noReference"),
          detail: report.without_reference.map(modelName).join(", "),
          level: "warning",
        });
      }
      if (report.analyzed === 0) {
        setSummary(
          report.without_reference.length > 0
            ? null
            : t("settings.advanced.learnedWords.nothingNew"),
        );
      } else {
        setSummary(
          [
            t("settings.advanced.learnedWords.done", {
              count: report.analyzed,
            }),
            learned.length > 0
              ? t("settings.advanced.learnedWords.found", {
                  words: learned.join(", "),
                })
              : t("settings.advanced.learnedWords.noneFound"),
            reinforced.length > 0
              ? t("settings.advanced.learnedWords.reinforced", {
                  words: reinforced.join(", "),
                })
              : "",
            suspected.length > 0
              ? t("settings.advanced.learnedWords.suspectsFound", {
                  words: suspected.join(", "),
                })
              : "",
            report.cancelled ? t("settings.advanced.learnedWords.stopped") : "",
          ]
            .filter(Boolean)
            .join(" "),
        );
      }
      setVocabulary(await commands.getLearnedVocabulary());
    } finally {
      setRunning(false);
      setProgress(null);
    }
  };

  const handleReference = async (value: string) => {
    applyResult(
      await commands.setVocabularyReferenceModel(
        value === AUTO_REFERENCE ? null : value,
      ),
    );
  };

  const startEditing = (word: string) => {
    setEditing(word);
    setDraft(word);
  };

  const saveEdit = async () => {
    const word = editing;
    const newWord = draft.trim();
    setEditing(null);
    if (!word || !newWord || newWord === word) return;
    applyResult(await commands.renameLearnedWord(currentModel, word, newWord));
  };

  const resolveSuspect = async (heardAs: string) => {
    const word = (answers[heardAs] ?? "").trim();
    if (!word) return;
    applyResult(
      await commands.resolveVocabularySuspect(currentModel, heardAs, word),
    );
    setAnswers(({ [heardAs]: _, ...rest }) => rest);
  };

  const progressText = progress
    ? t(`settings.advanced.learnedWords.phase.${progress.phase}`, {
        done: Math.min(progress.done + 1, progress.total),
        total: progress.total,
      })
    : t("settings.advanced.learnedWords.starting");

  const panelClass = `px-4 p-2 ${grouped ? "" : "rounded-lg border border-line"} flex flex-col gap-2`;

  return (
    <>
      <SettingContainer
        title={t("settings.advanced.learnedWords.title")}
        description={t("settings.advanced.learnedWords.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="stacked"
      >
        <div className="flex flex-wrap items-center gap-2">
          <Dropdown
            className="min-w-48"
            options={referenceOptions}
            selectedValue={vocabulary?.reference_model ?? AUTO_REFERENCE}
            onSelect={handleReference}
            disabled={running}
          />
          {running ? (
            <>
              <span className="inline-flex items-center gap-2 text-sm text-ink-2">
                <Spinner />
                {progressText}
              </span>
              <Button
                variant="secondary"
                size="md"
                onClick={() => commands.cancelLearnCustomWords()}
              >
                {t("settings.advanced.learnedWords.cancel")}
              </Button>
            </>
          ) : (
            <Button variant="primary" size="md" onClick={handleLearn}>
              {t("settings.advanced.learnedWords.learn")}
            </Button>
          )}
        </div>
        {summary && <p className="mt-2 text-sm text-ink-2">{summary}</p>}
      </SettingContainer>
      {learnedWords.length > 0 && (
        <div className={panelClass}>
          <div className="flex items-center justify-between text-xs text-ink-2">
            <span>
              {t("settings.advanced.learnedWords.forModel", {
                model: modelName(currentModel),
                count: learnedWords.length,
              })}
            </span>
            <Button
              variant="danger-ghost"
              size="sm"
              disabled={running}
              onClick={async () =>
                applyResult(await commands.clearLearnedWords(currentModel))
              }
            >
              {t("settings.advanced.learnedWords.clear")}
            </Button>
          </div>
          <div className="flex flex-wrap gap-1">
            {learnedWords.map((learned) =>
              editing === learned.word ? (
                <Input
                  key={learned.word}
                  type="text"
                  variant="compact"
                  className="max-w-48"
                  autoFocus
                  value={draft}
                  maxLength={MAX_WORD_CHARS}
                  onChange={(e) => setDraft(e.target.value)}
                  onBlur={saveEdit}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") saveEdit();
                    else if (e.key === "Escape") setEditing(null);
                  }}
                />
              ) : (
                <span
                  key={learned.word}
                  className="inline-flex items-center gap-1 rounded-md border border-line bg-wash px-2 py-0.5 text-sm"
                  title={[
                    t("settings.advanced.learnedWords.timesTooltip", {
                      count: learned.count,
                    }),
                    (learned.heard_as ?? []).length > 0
                      ? t("settings.advanced.learnedWords.heardAs", {
                          spellings: (learned.heard_as ?? []).join(", "),
                        })
                      : "",
                  ]
                    .filter(Boolean)
                    .join("\n")}
                >
                  <button
                    type="button"
                    className="cursor-text disabled:cursor-not-allowed"
                    disabled={running}
                    aria-label={t("settings.advanced.learnedWords.edit", {
                      word: learned.word,
                    })}
                    onClick={() => startEditing(learned.word)}
                  >
                    {learned.word}
                  </button>
                  <span className="text-xs font-semibold text-logo-primary">
                    ×{learned.count}
                  </span>
                  <button
                    type="button"
                    className="text-ink-3 hover:text-ink disabled:cursor-not-allowed"
                    disabled={running}
                    aria-label={t("settings.advanced.customWords.remove", {
                      word: learned.word,
                    })}
                    onClick={async () =>
                      applyResult(
                        await commands.removeLearnedWord(
                          currentModel,
                          learned.word,
                        ),
                      )
                    }
                  >
                    {removeIcon}
                  </button>
                </span>
              ),
            )}
          </div>
        </div>
      )}
      {suspects.length > 0 && (
        <div className={panelClass}>
          <div className="flex flex-col gap-0.5">
            <span className="text-xs text-ink-2">
              {t("settings.advanced.learnedWords.suspects.title", {
                count: suspects.length,
              })}
            </span>
            <span className="text-xs text-ink-3">
              {t("settings.advanced.learnedWords.suspects.description")}
            </span>
          </div>
          <div className="flex flex-col gap-2">
            {suspects.map((suspect) => (
              <div
                key={suspect.heard_as}
                className="flex flex-wrap items-center gap-2"
              >
                <div className="flex min-w-40 flex-1 flex-col">
                  <span className="text-sm">
                    <span className="font-semibold">{suspect.heard_as}</span>{" "}
                    <span
                      className="text-xs font-semibold text-logo-primary"
                      title={t("settings.advanced.learnedWords.suspects.seen", {
                        count: suspect.count,
                      })}
                    >
                      ×{suspect.count}
                    </span>
                  </span>
                  {suspect.context && (
                    <span
                      className="truncate text-xs italic text-ink-3"
                      title={suspect.context}
                    >
                      “{suspect.context}”
                    </span>
                  )}
                </div>
                <Input
                  type="text"
                  variant="compact"
                  className="max-w-40"
                  maxLength={MAX_WORD_CHARS}
                  disabled={running}
                  placeholder={t(
                    "settings.advanced.learnedWords.suspects.placeholder",
                  )}
                  value={answers[suspect.heard_as] ?? ""}
                  onChange={(e) =>
                    setAnswers((current) => ({
                      ...current,
                      [suspect.heard_as]: e.target.value,
                    }))
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Enter") resolveSuspect(suspect.heard_as);
                  }}
                />
                <Button
                  variant="primary"
                  size="sm"
                  disabled={
                    running || !(answers[suspect.heard_as] ?? "").trim()
                  }
                  onClick={() => resolveSuspect(suspect.heard_as)}
                >
                  {t("settings.advanced.learnedWords.suspects.save")}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={running}
                  onClick={async () =>
                    applyResult(
                      await commands.dismissVocabularySuspect(
                        currentModel,
                        suspect.heard_as,
                      ),
                    )
                  }
                >
                  {t("settings.advanced.learnedWords.suspects.dismiss")}
                </Button>
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  );
};
