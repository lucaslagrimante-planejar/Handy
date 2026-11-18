// Meeting mode in the recording overlay: the running meeting, its stop
// options, the call invite and the post-meeting card. Kept apart from
// RecordingOverlay.tsx (upstream) so syncing with upstream only meets the few
// hook lines there.
import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useRef, useState } from "react";
import type { TFunction } from "i18next";
import { commands } from "@/bindings";
import "./MeetingOverlay.css";

// The running meeting and the invite shown when a call is detected; then the
// post-meeting card: transcription finishing, the offer of the AI step, that
// step in progress, and its result.
export type MeetingOverlayState =
  | "meeting"
  | "stopmenu"
  | "invite"
  | "finishing"
  | "done"
  | "summarizing"
  | "summarized";

// Post-meeting card sent by the backend (`meeting-card`).
interface MeetingCard {
  name: string;
  note: string | null;
  // Part of "transcribe and summarize": the loading shows the step.
  chained: boolean;
}

const IS_MAC = navigator.userAgent.includes("Mac");

const inMeeting = (s: string | null) => s === "meeting" || s === "stopmenu";

/** Pieces of the upstream overlay the meeting views are built from. */
interface RenderContext {
  state: string;
  direction: React.HTMLAttributes<HTMLDivElement>["dir"];
  stageClass: string;
  waveform: React.ReactNode;
  elapsed: number;
  fmtTime: (s: number) => string;
  workingRow: (label: string, showCancel: boolean) => React.ReactNode;
}

interface Options {
  t: TFunction;
  state: string;
  isVisible: boolean;
  setElapsed: (s: number) => void;
  setCaptureReady: (ready: boolean) => void;
  resetLevels: () => void;
}

const meetingIcon = (
  <svg className="smeet-icon" viewBox="0 0 16 16" aria-hidden="true">
    <circle cx="5.5" cy="5" r="2.2" fill="currentColor" />
    <circle cx="11" cy="5.5" r="1.8" fill="currentColor" opacity="0.7" />
    <path
      d="M1.5 13 C1.5 10.2 3.3 8.8 5.5 8.8 C7.7 8.8 9.5 10.2 9.5 13 Z"
      fill="currentColor"
    />
    <path
      d="M10 13 C10 11.4 9.6 10.2 8.9 9.5 C9.5 9.2 10.2 9 11 9 C12.9 9 14.5 10.3 14.5 13 Z"
      fill="currentColor"
      opacity="0.7"
    />
  </svg>
);

const closeIcon = (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M4 4 L12 12 M12 4 L4 12"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
    />
  </svg>
);

const checkIcon = (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M3.5 8.5 L6.5 11.5 L12.5 4.5"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      fill="none"
    />
  </svg>
);

const sparkIcon = (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M8 1.5 L9.4 6.6 L14.5 8 L9.4 9.4 L8 14.5 L6.6 9.4 L1.5 8 L6.6 6.6 Z"
      fill="currentColor"
    />
  </svg>
);

const copyIcon = (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <rect
      x="5.5"
      y="5.5"
      width="8"
      height="8"
      rx="1.6"
      stroke="currentColor"
      strokeWidth="1.5"
      fill="none"
    />
    <path
      d="M10.5 3.5 V3.2 C10.5 2.5 10 2 9.3 2 H3.7 C3 2 2.5 2.5 2.5 3.2 V8.8 C2.5 9.5 3 10 3.7 10 H4"
      stroke="currentColor"
      strokeWidth="1.5"
      fill="none"
    />
  </svg>
);

const stopIcon = (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor" />
  </svg>
);

const folderIcon = (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M2 4.5 C2 3.7 2.6 3 3.4 3 H6.2 L7.6 4.5 H12.6 C13.4 4.5 14 5.2 14 6 V11.6 C14 12.4 13.4 13 12.6 13 H3.4 C2.6 13 2 12.4 2 11.6 Z"
      fill="currentColor"
    />
  </svg>
);

export function useMeetingOverlay({
  t,
  state,
  isVisible,
  setElapsed,
  setCaptureReady,
  resetLevels,
}: Options) {
  // App of the call detected (for the invite), and whether to highlight the
  // meeting button of the dictation in progress because of it.
  const [inviteApp, setInviteApp] = useState("");
  const [callSuggested, setCallSuggested] = useState(false);
  // Meeting the post-meeting card refers to.
  const [card, setCard] = useState<MeetingCard>({
    name: "",
    note: null,
    chained: false,
  });
  // Last state shown, read by the show handler (registered once).
  const stateRef = useRef<string | null>(null);
  // "Copied" feedback on the summary button.
  const [copied, setCopied] = useState(false);
  // Participant names typed before the AI step, and whether the overlay has
  // the keyboard for that field right now.
  const [participants, setParticipants] = useState("");
  const participantsRef = useRef<HTMLInputElement>(null);
  const keyboardRef = useRef(false);
  const cardNameRef = useRef("");

  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;
    const setup = async () => {
      const subscriptions = await Promise.all([
        listen("show-overlay", (event) => {
          const overlayState = event.payload as string;
          if (inMeeting(overlayState)) {
            // Levels come from the meeting capture, which is already running.
            // Going between the pill and the stop menu keeps the timer as is.
            if (!inMeeting(stateRef.current)) {
              resetLevels();
              setElapsed(0);
              setParticipants("");
            }
            setCaptureReady(true);
            commands
              .getMeetingStatus()
              .then((status) => setElapsed(status.elapsed_secs))
              .catch(() => {});
          }
          stateRef.current = overlayState;
        }),
        listen("hide-overlay", () => setCallSuggested(false)),
        listen<string>("meeting-invite", (event) => {
          setInviteApp(event.payload);
          setCallSuggested(true);
        }),
        listen<MeetingCard>("meeting-card", (event) => {
          setCard(event.payload);
          setCopied(false);
          // A meeting picked up again (e.g. from History) shows the names
          // saved with it.
          if (event.payload.name !== cardNameRef.current) {
            cardNameRef.current = event.payload.name;
            commands
              .getMeetingParticipants(event.payload.name)
              .then((result) => {
                if (result.status === "ok" && result.data.length > 0) {
                  setParticipants(result.data.join(", "));
                }
              })
              .catch(() => {});
          }
        }),
      ]);
      if (disposed) subscriptions.forEach((unlisten) => unlisten());
      else unlisteners.push(...subscriptions);
    };
    setup();
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
    // The setters passed in are stable for the overlay's lifetime.
  }, []);

  // Give the keyboard back once the participants field is gone.
  useEffect(() => {
    if (isVisible && (state === "stopmenu" || state === "done")) return;
    if (keyboardRef.current) {
      keyboardRef.current = false;
      commands.setOverlayKeyboard(false).catch(() => {});
    }
  }, [isVisible, state]);

  // Switches the dictation in progress to a meeting (its audio comes along).
  // Highlighted when a call was detected during the dictation.
  const meetingBtn = (
    <button
      className={`sx smeet ${callSuggested ? "suggest" : ""}`}
      aria-label={t("overlay.meeting.start")}
      title={
        callSuggested
          ? t("overlay.meeting.suggest")
          : t("overlay.meeting.start")
      }
      onClick={() => commands.convertToMeeting()}
    >
      {meetingIcon}
    </button>
  );

  const dismissBtn = (
    <button
      className="sx"
      aria-label={t("overlay.meeting.dismiss")}
      title={t("overlay.meeting.dismiss")}
      onClick={() => commands.dismissMeetingInvite()}
    >
      {closeIcon}
    </button>
  );

  // Participant names for the AI step. The overlay never holds the keyboard,
  // so it asks for it on press and gives it back on blur. `submit` runs on
  // Enter. Not on macOS, whose overlay panel can't take the keyboard.
  const participantsField = (submit: () => void) =>
    IS_MAC ? null : (
      <input
        ref={participantsRef}
        className="sparticipants"
        type="text"
        value={participants}
        placeholder={t("overlay.meeting.participants")}
        title={t("overlay.meeting.participantsHint")}
        aria-label={t("overlay.meeting.participantsHint")}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => setParticipants(e.target.value)}
        onPointerDown={() => {
          if (keyboardRef.current) return;
          keyboardRef.current = true;
          commands
            .setOverlayKeyboard(true)
            .then(() => participantsRef.current?.focus())
            .catch(() => {});
        }}
        onBlur={() => {
          if (!keyboardRef.current) return;
          keyboardRef.current = false;
          commands.setOverlayKeyboard(false).catch(() => {});
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
          else if (e.key === "Escape") participantsRef.current?.blur();
        }}
      />
    );

  /** The view for a meeting state, or `null` for the upstream states. */
  const renderMeeting = ({
    state,
    direction,
    stageClass,
    waveform,
    elapsed,
    fmtTime,
    workingRow,
  }: RenderContext): React.ReactElement | null => {
    // ---- Meeting: icon | waveform | timer + stop. Fixed and compact; the
    // transcribe shortcut also ends it.
    if (state === "meeting") {
      return (
        <div dir={direction} className={stageClass}>
          <div className="scard compact cmeeting">
            <div className="sbase">
              <div className="sbase-l smeet-live">{meetingIcon}</div>
              {waveform}
              <div className="sbase-r">
                <span className="stimer">{fmtTime(elapsed)}</span>
                <button
                  className="sx sstop"
                  aria-label={t("overlay.meeting.stop")}
                  title={t("overlay.meeting.stop")}
                  onClick={() => commands.showMeetingStopMenu(true)}
                >
                  {stopIcon}
                </button>
              </div>
            </div>
          </div>
        </div>
      );
    }

    // ---- Stop options of the running meeting: what to do once it ends.
    if (state === "stopmenu") {
      return (
        <div dir={direction} className={stageClass}>
          <div className="scard compact cresult cstop">
            <div className="sresult-head">
              <span className="sresult-badge smeet-live">{meetingIcon}</span>
              <div className="sresult-text">
                <span className="sresult-title">
                  {t("overlay.meeting.stopTitle")}
                </span>
                <span className="sresult-note">
                  {t("overlay.meeting.stopRecorded", {
                    time: fmtTime(elapsed),
                  })}
                </span>
              </div>
              <button
                className="sx"
                aria-label={t("overlay.meeting.keepRecording")}
                title={t("overlay.meeting.keepRecording")}
                onClick={() => commands.showMeetingStopMenu(false)}
              >
                {closeIcon}
              </button>
            </div>
            <div className="sstop-options">
              {participantsField(() =>
                commands.stopMeeting("summarize", participants),
              )}
              <button
                className="sopt primary"
                onClick={() => commands.stopMeeting("summarize", participants)}
              >
                {sparkIcon}
                {t("overlay.meeting.stopSummarize")}
              </button>
              <button
                className="sopt"
                onClick={() => commands.stopMeeting("transcribe", participants)}
              >
                {checkIcon}
                {t("overlay.meeting.stopTranscribe")}
              </button>
              <button
                className="sopt plain"
                title={t("overlay.meeting.stopNothingHint")}
                onClick={() => commands.stopMeeting("nothing", participants)}
              >
                {stopIcon}
                {t("overlay.meeting.stopNothing")}
              </button>
            </div>
          </div>
        </div>
      );
    }

    // ---- Call invite: label + action + dismiss.
    if (state === "invite") {
      return (
        <div dir={direction} className={stageClass}>
          <div className="scard compact cprompt">
            <div className="sprompt">
              <span className="smeet-live">{meetingIcon}</span>
              <span className="sprompt-label">
                {t("overlay.meeting.invite", { app: inviteApp })}
              </span>
              <button className="sbtn" onClick={() => commands.startMeeting()}>
                {t("overlay.meeting.inviteAction")}
              </button>
              {dismissBtn}
            </div>
          </div>
        </div>
      );
    }

    // ---- Post-meeting work in progress: spinner + label until it finishes.
    if (state === "finishing" || state === "summarizing") {
      return (
        <div dir={direction} className={stageClass}>
          <div className="scard compact cworking cwide">
            {workingRow(
              (state === "finishing"
                ? t("overlay.meeting.finishing")
                : t("overlay.meeting.summarizing")) +
                (card.chained
                  ? state === "finishing"
                    ? " · 1/2"
                    : " · 2/2"
                  : ""),
              false,
            )}
          </div>
        </div>
      );
    }

    // ---- Post-meeting card: title + note, and a row of actions. Stays up
    // until the user picks one.
    if (state === "done" || state === "summarized") {
      const done = state === "done";
      const summarize = () =>
        commands.summarizeMeeting(card.name, participants);
      const copySummary = async () => {
        const result = await commands.copyMeetingSummary(card.name);
        if (result.status === "ok") {
          setCopied(true);
          setTimeout(() => setCopied(false), 1600);
        }
      };
      return (
        <div dir={direction} className={stageClass}>
          <div className="scard compact cresult">
            <div className="sresult-head">
              <span className="sresult-badge">{checkIcon}</span>
              <div className="sresult-text">
                <span className="sresult-title">
                  {done
                    ? t("overlay.meeting.done")
                    : t("overlay.meeting.summarized")}
                </span>
                {card.note && (
                  <span className="sresult-note" title={card.note}>
                    {card.note}
                  </span>
                )}
              </div>
              {dismissBtn}
            </div>
            {done && (
              <div className="sresult-field">
                {participantsField(summarize)}
              </div>
            )}
            <div className="sresult-actions">
              {done ? (
                <button className="sbtn" onClick={summarize}>
                  {sparkIcon}
                  <span>{t("overlay.meeting.summarize")}</span>
                </button>
              ) : (
                <button className="sbtn" onClick={copySummary}>
                  {copied ? checkIcon : copyIcon}
                  <span>
                    {copied
                      ? t("overlay.meeting.copied")
                      : t("overlay.meeting.copySummary")}
                  </span>
                </button>
              )}
              <button
                className="sbtn ghost"
                onClick={() => commands.openMeetingFolder(card.name)}
              >
                {folderIcon}
                <span>{t("overlay.meeting.openFolder")}</span>
              </button>
            </div>
          </div>
        </div>
      );
    }

    return null;
  };

  return { meetingBtn, renderMeeting };
}
