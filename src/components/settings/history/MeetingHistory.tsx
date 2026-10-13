import React, { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  Check,
  CircleDashed,
  Copy,
  FileText,
  FolderOpen,
  Loader2,
  RotateCcw,
  Sparkles,
  Trash2,
  Users,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  type AiProviderInfo,
  type MeetingInfo,
  type MeetingState,
} from "@/bindings";
import { formatDateTime } from "@/utils/dateFormat";
import { Button } from "../../ui/Button";
import { copyToClipboard } from "./clipboard";
import { CollapsibleSection } from "./CollapsibleSection";

const MEETINGS_CHANGED = "meetings-changed";

/** Files written by the AI step, in the order their buttons appear. */
const AI_DOCS = [
  { file: "resumo.md", key: "summary" },
  { file: "ata.md", key: "minutes" },
  { file: "transcricao-revisada.md", key: "revised" },
  { file: "transcricao-revisada.srt", key: "revisedSrt" },
] as const;

/** The untouched transcript, always kept. */
const ORIGINAL_DOCS = [
  { file: "transcript-final.md", key: "original" },
  { file: "transcript.srt", key: "originalSrt" },
] as const;

const formatDuration = (secs: number) => {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h}h${String(m).padStart(2, "0")}`;
  if (m > 0) return `${m} min`;
  return `${s} s`;
};

const unwrap = <T,>(
  result: { status: "ok"; data: T } | { status: "error"; error: string },
): T => {
  if (result.status === "error") throw new Error(result.error);
  return result.data;
};

const IconButton: React.FC<{
  onClick: () => void;
  title: string;
  disabled?: boolean;
  children: React.ReactNode;
}> = ({ onClick, title, disabled, children }) => (
  <button
    onClick={onClick}
    disabled={disabled}
    title={title}
    className="p-1.5 rounded-md flex items-center justify-center transition-colors cursor-pointer text-text/50 hover:text-logo-primary disabled:cursor-not-allowed disabled:text-text/20"
  >
    {children}
  </button>
);

const Step: React.FC<{ done: boolean; busy?: boolean; label: string }> = ({
  done,
  busy,
  label,
}) => (
  <span
    className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs border ${
      done
        ? "border-mid-gray/20 text-text/60"
        : "border-logo-primary/50 text-logo-primary bg-logo-primary/10"
    }`}
  >
    {busy ? (
      <Loader2 className="w-3 h-3 animate-spin" />
    ) : done ? (
      <Check className="w-3 h-3" />
    ) : (
      <CircleDashed className="w-3 h-3" />
    )}
    {label}
  </span>
);

export const MeetingHistory: React.FC = () => {
  const { t } = useTranslation();
  const [meetings, setMeetings] = useState<MeetingInfo[]>([]);
  const [provider, setProvider] = useState<AiProviderInfo | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [list, info] = await Promise.all([
        commands.listMeetings(),
        commands.getMeetingAiProvider(),
      ]);
      setMeetings(list);
      setProvider(info);
    } catch (error) {
      console.error("Failed to load meetings:", error);
    }
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = listen(MEETINGS_CHANGED, () => refresh());
    // The provider may change in the post-processing settings.
    window.addEventListener("focus", refresh);
    return () => {
      unlisten.then((fn) => fn());
      window.removeEventListener("focus", refresh);
    };
  }, [refresh]);

  if (meetings.length === 0) return null;

  return (
    <CollapsibleSection
      id="meetings"
      title={t("settings.history.meetings.title")}
      count={meetings.length}
      actions={
        <Button
          onClick={() => commands.openMeetingsFolder()}
          variant="secondary"
          size="sm"
          className="flex items-center gap-2"
        >
          <FolderOpen className="w-4 h-4" />
          <span>{t("settings.history.meetings.openFolder")}</span>
        </Button>
      }
    >
      <div className="bg-background border border-mid-gray/20 rounded-lg divide-y divide-mid-gray/20">
        {meetings.map((meeting) => (
          <MeetingEntry
            key={meeting.name}
            meeting={meeting}
            provider={provider}
            onChanged={refresh}
          />
        ))}
      </div>
    </CollapsibleSection>
  );
};

const MeetingEntry: React.FC<{
  meeting: MeetingInfo;
  provider: AiProviderInfo | null;
  onChanged: () => void;
}> = ({ meeting, provider, onChanged }) => {
  const { t, i18n } = useTranslation();
  const [copied, setCopied] = useState(false);
  const { name, state, files } = meeting;
  const has = (file: string) => files.includes(file);
  const transcribed = ["transcribed", "summarizing", "summarized"].includes(
    state,
  );
  const busy: MeetingState[] = ["recording", "finishing", "summarizing"];
  const isBusy = busy.includes(state);
  const aiFiles = AI_DOCS.filter((doc) => has(doc.file));
  const providerReady =
    provider !== null && provider.model.trim() !== "" && provider.has_key;

  const run = async (action: () => Promise<unknown>, errorKey: string) => {
    try {
      await action();
    } catch (error) {
      console.error(error);
      toast.error(t(errorKey, { error: String(error) }));
    } finally {
      onChanged();
    }
  };

  const copyTranscript = () =>
    run(async () => {
      const text = unwrap(await commands.getMeetingTranscript(name));
      if (!(await copyToClipboard(text))) throw new Error("clipboard");
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }, "settings.history.copyError");

  const generate = () =>
    run(
      async () => unwrap(await commands.summarizeMeeting(name)),
      "settings.history.meetings.aiError",
    );

  const deleteAi = async (thenGenerate: boolean) => {
    const confirmed = await ask(
      t(
        thenGenerate
          ? "settings.history.meetings.redoConfirm"
          : "settings.history.meetings.deleteAiConfirm",
        { model: provider ? `${provider.provider} · ${provider.model}` : "" },
      ),
      { title: t("settings.history.meetings.deleteAi"), kind: "warning" },
    );
    if (!confirmed) return;
    await run(async () => {
      unwrap(await commands.deleteMeetingAiFiles(name));
      if (thenGenerate) unwrap(await commands.summarizeMeeting(name));
    }, "settings.history.meetings.aiError");
  };

  const deleteMeeting = async () => {
    const confirmed = await ask(t("settings.history.meetings.deleteConfirm"), {
      title: t("settings.history.meetings.delete"),
      kind: "warning",
    });
    if (!confirmed) return;
    await run(
      async () => unwrap(await commands.deleteMeeting(name)),
      "settings.history.deleteError",
    );
  };

  const finalize = () =>
    run(
      async () => unwrap(await commands.finalizeMeeting(name)),
      "settings.history.meetings.finalizeError",
    );

  const openFile = (file: string) =>
    run(
      async () => unwrap(await commands.openMeetingFile(name, file)),
      "settings.history.meetings.openError",
    );

  const docButton = (file: string, label: string, highlight = false) => (
    <button
      key={file}
      onClick={() => openFile(file)}
      className={`inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs border cursor-pointer transition-colors ${
        highlight
          ? "border-logo-primary/40 text-text hover:bg-logo-primary/10"
          : "border-mid-gray/20 text-text/70 hover:text-logo-primary"
      }`}
    >
      <FileText className="w-3.5 h-3.5" />
      {label}
    </button>
  );

  const providerLine = provider
    ? t("settings.history.meetings.provider", {
        provider: provider.provider,
        model: provider.model || "—",
      })
    : null;

  return (
    <div className="px-4 py-3 pb-5 flex flex-col gap-3">
      <div className="flex justify-between items-center gap-2">
        <div className="flex items-center gap-2 min-w-0">
          <Users className="w-4 h-4 text-logo-primary shrink-0" />
          <p className="text-sm font-medium truncate">
            {formatDateTime(String(meeting.timestamp), i18n.language)}
          </p>
          <span className="text-xs text-text/50 shrink-0">
            {formatDuration(meeting.duration_secs)} ·{" "}
            {meeting.speakers.join(", ")}
          </span>
        </div>
        <div className="flex items-center shrink-0">
          <IconButton
            onClick={copyTranscript}
            title={t("settings.history.meetings.copy")}
          >
            {copied ? (
              <Check width={16} height={16} />
            ) : (
              <Copy width={16} height={16} />
            )}
          </IconButton>
          <IconButton
            onClick={() => commands.openMeetingFolder(name)}
            title={t("settings.history.meetings.openMeetingFolder")}
          >
            <FolderOpen width={16} height={16} />
          </IconButton>
          <IconButton
            onClick={deleteMeeting}
            disabled={isBusy}
            title={t("settings.history.meetings.delete")}
          >
            <Trash2 width={16} height={16} />
          </IconButton>
        </div>
      </div>

      <div className="flex flex-wrap gap-1.5">
        <Step
          done={transcribed}
          busy={state === "recording" || state === "finishing"}
          label={t(`settings.history.meetings.state.${state}`)}
        />
        {transcribed && (
          <Step
            done={meeting.diarized}
            label={t(
              meeting.diarized
                ? "settings.history.meetings.diarized"
                : "settings.history.meetings.notDiarized",
            )}
          />
        )}
        {transcribed &&
          AI_DOCS.slice(0, 3).map((doc) => (
            <Step
              key={doc.file}
              done={has(doc.file)}
              busy={state === "summarizing"}
              label={t(`settings.history.meetings.docs.${doc.key}`)}
            />
          ))}
      </div>

      {meeting.preview && (
        <p className="italic text-sm text-text/80 whitespace-pre-wrap break-words line-clamp-4 select-text cursor-text">
          {meeting.preview}
        </p>
      )}

      {state === "unfinished" && (
        <div className="rounded-lg border border-logo-primary/40 bg-logo-primary/5 p-3 flex items-center justify-between gap-3">
          <p className="text-xs text-text/80">
            {t("settings.history.meetings.unfinished")}
          </p>
          <Button size="sm" onClick={finalize}>
            {t("settings.history.meetings.finalize")}
          </Button>
        </div>
      )}

      {transcribed && !meeting.diarized && state !== "summarizing" && (
        <button
          onClick={finalize}
          className="self-start text-xs text-text/60 hover:text-logo-primary underline cursor-pointer"
        >
          {t("settings.history.meetings.retryDiarization")}
        </button>
      )}

      {state === "transcribed" && aiFiles.length === 0 && (
        <div className="rounded-lg border border-logo-primary/50 bg-logo-primary/10 p-3 flex flex-col gap-2">
          <div className="flex items-center justify-between gap-3">
            <p className="text-sm font-medium">
              {t("settings.history.meetings.aiPending")}
            </p>
            <Button
              size="sm"
              onClick={generate}
              disabled={!providerReady}
              className="flex items-center gap-1.5 shrink-0"
            >
              <Sparkles className="w-4 h-4" />
              {t("settings.history.meetings.generate")}
            </Button>
          </div>
          <p className="text-xs text-text/60">
            {providerReady
              ? providerLine
              : t("settings.history.meetings.providerMissing")}{" "}
            {t("settings.history.meetings.textOnly")}
          </p>
        </div>
      )}

      {state === "summarizing" && (
        <div className="rounded-lg border border-logo-primary/40 p-3 flex items-center gap-2 text-sm">
          <Loader2 className="w-4 h-4 animate-spin text-logo-primary" />
          {t("settings.history.meetings.generating")}
          {providerLine && (
            <span className="text-xs text-text/50">· {providerLine}</span>
          )}
        </div>
      )}

      {transcribed && (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap gap-1.5">
            {aiFiles.map((doc) =>
              docButton(
                doc.file,
                t(`settings.history.meetings.docs.${doc.key}`),
                true,
              ),
            )}
            {ORIGINAL_DOCS.filter((doc) => has(doc.file)).map((doc) =>
              docButton(
                doc.file,
                t(`settings.history.meetings.docs.${doc.key}`),
              ),
            )}
          </div>
          {aiFiles.length > 0 && state !== "summarizing" && (
            <div className="flex flex-wrap items-center gap-3 text-xs">
              {state === "transcribed" && (
                <button
                  onClick={generate}
                  disabled={!providerReady}
                  className="inline-flex items-center gap-1 text-logo-primary hover:underline cursor-pointer disabled:text-text/30"
                >
                  <Sparkles className="w-3.5 h-3.5" />
                  {t("settings.history.meetings.complete")}
                </button>
              )}
              <button
                onClick={() => deleteAi(true)}
                disabled={!providerReady}
                className="inline-flex items-center gap-1 text-text/60 hover:text-logo-primary cursor-pointer disabled:text-text/30"
              >
                <RotateCcw className="w-3.5 h-3.5" />
                {t("settings.history.meetings.redo")}
              </button>
              <button
                onClick={() => deleteAi(false)}
                className="inline-flex items-center gap-1 text-text/60 hover:text-red-500 cursor-pointer"
              >
                <Trash2 className="w-3.5 h-3.5" />
                {t("settings.history.meetings.deleteAi")}
              </button>
              {providerLine && (
                <span className="text-text/40">{providerLine}</span>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
};
