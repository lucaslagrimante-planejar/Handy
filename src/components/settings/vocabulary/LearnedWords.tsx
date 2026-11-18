// Fork feature: custom words learned per model from the dictation history
// (see src-tauri/src/vocabulary). Shown under the user's own custom words.
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { commands, type LearnedVocabulary } from "@/bindings";
import { notify } from "@/stores/noticeStore";
import { useModelStore } from "@/stores/modelStore";
import { Button } from "../../ui/Button";
import { Dropdown } from "../../ui/Dropdown";
import { SettingContainer } from "../../ui/SettingContainer";
import { Spinner } from "../../ui/Spinner";

/** `PROGRESS_EVENT` in vocabulary/learn.rs. */
const PROGRESS_EVENT = "vocabulary-learning-progress";
const AUTO_REFERENCE = "__auto__";

interface LearnProgress {
  phase: "transcribing" | "comparing";
  done: number;
  total: number;
}

interface LearnedWordsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

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

  const learnedWords = vocabulary?.models?.[currentModel] ?? [];

  const applyResult = (
    result:
      | { status: "ok"; data: LearnedVocabulary }
      | { status: "error"; error: string },
  ) => {
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
      const learned = Object.values(report.learned).flatMap(
        (words) => words ?? [],
      );
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

  const progressText = progress
    ? t(`settings.advanced.learnedWords.phase.${progress.phase}`, {
        done: Math.min(progress.done + 1, progress.total),
        total: progress.total,
      })
    : t("settings.advanced.learnedWords.starting");

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
        <div
          className={`px-4 p-2 ${grouped ? "" : "rounded-lg border border-line"} flex flex-col gap-2`}
        >
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
            {learnedWords.map((word) => (
              <Button
                key={word}
                onClick={async () =>
                  applyResult(
                    await commands.removeLearnedWord(currentModel, word),
                  )
                }
                disabled={running}
                variant="secondary"
                size="sm"
                className="inline-flex items-center gap-1 cursor-pointer"
                aria-label={t("settings.advanced.customWords.remove", {
                  word,
                })}
              >
                <span>{word}</span>
                <svg
                  className="w-3 h-3"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M6 18L18 6M6 6l12 12"
                  />
                </svg>
              </Button>
            ))}
          </div>
        </div>
      )}
    </>
  );
};
