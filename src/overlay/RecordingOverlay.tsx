import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import "./RecordingOverlay.css";
import { commands, events } from "@/bindings";
import type {
  StreamPhase,
  StreamPhaseEvent,
  StreamTextEvent,
  StreamWorkKind,
} from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import type { ModelStateEvent } from "@/lib/types/events";
import { getLanguageDirection } from "@/lib/utils/rtl";

type OverlayState =
  | "recording"
  | "streaming"
  | "transcribing"
  | "processing"
  // Meeting mode: the running meeting and the invite shown when a call is
  // detected; then the post-meeting card: transcription finishing, the offer
  // of the AI step, that step in progress, and its result.
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

// Number of reactive bars in the waveform (the simple, smoothed style shared by
// every overlay form). Mic levels arrive as 16 FFT buckets; we take the first N.
const WAVE_BARS = 9;
// Slack (CSS px) around the card where the overlay still takes clicks.
const HIT_REGION_PAD = 6;

// Only call out a model load in the Live preview once it has run this long.
// Warm loads finish in well under this (~0.2s on Apple Silicon, ~1.5s on a
// Windows CPU backend), so the common case never flashes a loading notice while
// the user is speaking; cold loads can take 40s+.
const SLOW_MODEL_LOAD_MS = 2000;

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  // `Stream::play()` returning does not mean hardware callbacks are flowing.
  // Stay visually in an arming state until the backend processes the first
  // actual microphone sample chunk.
  const [captureReady, setCaptureReady] = useState(false);
  // Recording starts while the model loads in the background. Say so, otherwise
  // a cold start looks like an empty Live preview that ignores speech, then a
  // hung "Transcribing..." spinner. Once recording stops, any in-flight load is
  // the wait, so the working label reflects it immediately; the Live notice
  // waits for the load to be slow so fast loads don't flash it mid-speech.
  const [modelLoading, setModelLoading] = useState(false);
  const [modelLoadSlow, setModelLoadSlow] = useState(false);
  const modelLoadTimerRef = useRef<ReturnType<typeof setTimeout>>();
  // Latched per Live session: once the loading notice has opened the panel, keep
  // it open after the load so it doesn't collapse and reopen when text arrives.
  const [loadNoticeShown, setLoadNoticeShown] = useState(false);
  const [levels, setLevels] = useState<number[]>(Array(WAVE_BARS).fill(0));
  const [streamText, setStreamText] = useState<StreamTextEvent>({
    committed: "",
    tentative: "",
  });
  const [phase, setPhase] = useState<StreamPhase>("listening");
  const [workKind, setWorkKind] = useState<StreamWorkKind>("transcribing");
  const [elapsed, setElapsed] = useState(0);
  // Bumped on each new streaming session so the Live card remounts fresh (replays
  // the pop-in, and never animates in from the previous panel's open size).
  const [session, setSession] = useState(0);
  // Overlay placement (top vs bottom of the screen). The Live panel grows downward
  // from a top overlay (oldest line under the pill) and upward from a bottom one.
  const [position, setPosition] = useState<"top" | "bottom">("bottom");
  // True once live text overflows the cap. A top overlay fades its top edge only
  // while overflowing, so the resting first line stays crisp flush under the pill.
  const [overflowing, setOverflowing] = useState(false);
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
  const stateRef = useRef<OverlayState | null>(null);
  // "Copied" feedback on the summary button.
  const [copied, setCopied] = useState(false);

  const smoothedLevelsRef = useRef<number[]>(Array(16).fill(0));
  // Live-text scroll-back: the text region "sticks" to the newest line while the
  // user is at the bottom; if they scroll up to read history, auto-follow pauses
  // until they scroll back down.
  const capRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);
  const direction = getLanguageDirection(i18n.language);

  useEffect(() => {
    const setupEventListeners = async () => {
      const unlistenShow = await listen("show-overlay", async (event) => {
        const overlayState = event.payload as OverlayState;
        // Reset synchronously before settings I/O. A fast microphone can emit
        // recording-ready while the awaits below are in flight; resetting after
        // them would overwrite that event and leave the overlay stuck arming.
        if (overlayState === "recording" || overlayState === "streaming") {
          setCaptureReady(false);
          smoothedLevelsRef.current = Array(16).fill(0);
          setLevels(Array(WAVE_BARS).fill(0));
          setStreamText({ committed: "", tentative: "" });
        }

        await syncLanguageFromSettings();
        // The Live panel flows downward from a top overlay and upward from a
        // bottom one; read the placement so the layout can flip to match.
        try {
          const settings = await commands.getAppSettings();
          if (settings.status === "ok") {
            setPosition(
              settings.data.overlay_position === "top" ? "top" : "bottom",
            );
          }
        } catch {
          // Keep the previous/default placement if settings can't be read.
        }
        const inMeeting = (s: OverlayState | null) =>
          s === "meeting" || s === "stopmenu";
        if (inMeeting(overlayState)) {
          // Levels come from the meeting capture, which is already running.
          // Going between the pill and the stop menu keeps the timer as is.
          if (!inMeeting(stateRef.current)) {
            smoothedLevelsRef.current = Array(16).fill(0);
            setLevels(Array(WAVE_BARS).fill(0));
            setElapsed(0);
          }
          setCaptureReady(true);
          commands
            .getMeetingStatus()
            .then((status) => setElapsed(status.elapsed_secs))
            .catch(() => {});
        }
        stateRef.current = overlayState;
        setState(overlayState);
        if (overlayState === "streaming") {
          setPhase("listening");
          setWorkKind("transcribing");
          setElapsed(0);
          setLoadNoticeShown(false);
          setSession((s) => s + 1); // remount the card fresh for this session
        }
        setIsVisible(true);
      });

      const unlistenHide = await listen("hide-overlay", () => {
        setIsVisible(false);
        setCaptureReady(false);
        setCallSuggested(false);
      });

      const unlistenInvite = await listen<string>("meeting-invite", (event) => {
        setInviteApp(event.payload);
        setCallSuggested(true);
      });

      const unlistenCard = await listen<MeetingCard>(
        "meeting-card",
        (event) => {
          setCard(event.payload);
          setCopied(false);
        },
      );

      const unlistenReady = await listen("recording-ready", () => {
        setElapsed(0);
        setCaptureReady(true);
      });

      const unlistenLevel = await listen<number[]>("mic-level", (event) => {
        const newLevels = event.payload as number[];
        // Exponential smoothing across the 16 buckets, then take the first N
        // bars for the shared waveform.
        const smoothed = smoothedLevelsRef.current.map((prev, i) => {
          const target = newLevels[i] || 0;
          return prev * 0.7 + target * 0.3;
        });
        smoothedLevelsRef.current = smoothed;
        setLevels(smoothed.slice(0, WAVE_BARS));
      });

      const unlistenStream = await events.streamTextEvent.listen((event) => {
        setStreamText(event.payload);
      });

      const unlistenPhase = await events.streamPhaseEvent.listen((event) => {
        const payload: StreamPhaseEvent = event.payload;
        setPhase(payload.phase);
        if (payload.kind) setWorkKind(payload.kind);
      });

      // The backend ends every `loading_started` with exactly one of completed
      // or failed, so only those end a load. Other events (e.g. `unloaded` from
      // the idle watcher) aren't ordered against an in-flight load and must not
      // clear it.
      const unlistenModel = await listen<ModelStateEvent>(
        "model-state-changed",
        (event) => {
          const type = event.payload.event_type;
          if (type === "loading_started") {
            clearTimeout(modelLoadTimerRef.current);
            setModelLoading(true);
            setModelLoadSlow(false);
            modelLoadTimerRef.current = setTimeout(
              () => setModelLoadSlow(true),
              SLOW_MODEL_LOAD_MS,
            );
          } else if (
            type === "loading_completed" ||
            type === "loading_failed"
          ) {
            clearTimeout(modelLoadTimerRef.current);
            setModelLoading(false);
            setModelLoadSlow(false);
          }
        },
      );

      return () => {
        unlistenShow();
        unlistenHide();
        unlistenInvite();
        unlistenCard();
        unlistenReady();
        unlistenLevel();
        unlistenStream();
        unlistenPhase();
        unlistenModel();
        clearTimeout(modelLoadTimerRef.current);
      };
    };

    setupEventListeners();
  }, []);

  // Elapsed capture timer starts only once microphone samples are flowing.
  useEffect(() => {
    if (state !== "streaming" && state !== "meeting" && state !== "stopmenu")
      return;
    if (!isVisible || !captureReady) return;
    const id = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(id);
  }, [state, isVisible, captureReady]);

  // Stick to the bottom as text streams in — but only while pinned, so a user who
  // has scrolled up to read history isn't yanked back down by the next chunk.
  useLayoutEffect(() => {
    const el = capRef.current;
    if (!el) return;
    // Fade the top edge only once text actually overflows the cap.
    setOverflowing(el.scrollHeight > el.clientHeight + 1);
    if (pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [streamText]);

  // `session` re-runs this when a new Live session starts mid-load (e.g. a
  // retry during a cold load), since the reset doesn't change the other deps.
  useEffect(() => {
    if (modelLoadSlow && state === "streaming") setLoadNoticeShown(true);
  }, [modelLoadSlow, state, session]);

  // Each fresh streaming session starts pinned to the bottom, fade cleared.
  useEffect(() => {
    pinnedRef.current = true;
    setOverflowing(false);
  }, [session]);

  // The overlay window is a transparent rectangle bigger than the card, so tell
  // the backend to take clicks only over the card; the rest of the window lets
  // them through to whatever is below. While hidden, nothing takes clicks.
  useEffect(() => {
    let last = "";
    const send = (x: number, y: number, width: number, height: number) => {
      const key = `${x},${y},${width},${height}`;
      if (key === last) return;
      last = key;
      commands.setOverlayHitRegion(x, y, width, height).catch(() => {});
    };
    if (!isVisible) {
      send(0, 0, 0, 0);
      return;
    }

    let frame = 0;
    const report = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const cardEl = document.querySelector(".scard");
        if (!cardEl) return send(0, 0, 0, 0);
        // A little slack for the pop-in scale and the button hover outlines.
        const r = cardEl.getBoundingClientRect();
        const pad = HIT_REGION_PAD;
        send(r.left - pad, r.top - pad, r.width + pad * 2, r.height + pad * 2);
      });
    };

    const observer = new ResizeObserver(report);
    const cardEl = document.querySelector(".scard");
    if (cardEl) observer.observe(cardEl);
    // Transforms and pop-ins don't resize the box, so re-measure after them.
    document.addEventListener("transitionend", report);
    document.addEventListener("animationend", report);
    window.addEventListener("resize", report);
    report();
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      document.removeEventListener("transitionend", report);
      document.removeEventListener("animationend", report);
      window.removeEventListener("resize", report);
    };
  }, [isVisible, state, session, position]);

  if (!isVisible) return null;

  // Re-pin when the user is within ~a line of the bottom; unpin otherwise.
  const handleStreamScroll = () => {
    const el = capRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= 16;
  };

  const fmtTime = (s: number) =>
    `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

  const transcribingLabel = modelLoading
    ? t("overlay.loadingModel")
    : t("overlay.transcribing");

  // ---- Shared building blocks (one visual language for every overlay form) ----
  const waveform = (
    <div className={`swave ${captureReady ? "ready" : "arming"}`}>
      {levels.map((v, i) => (
        <i
          key={i}
          style={{
            height: `${Math.max(3, Math.min(18, 3 + Math.pow(v, 0.7) * 15))}px`,
          }}
        />
      ))}
    </div>
  );

  const cancelBtn = (
    <button
      className="sx"
      aria-label="cancel"
      onClick={() => commands.cancelOperation()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M4 4 L12 12 M12 4 L4 12"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
        />
      </svg>
    </button>
  );

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

  // dot (left) | waveform (center) | timer + meeting + cancel (right) — same
  // structure for pill & panel, so the Live morph is a pure width change.
  const listeningRow = (showTimer: boolean, showCancel: boolean) => (
    <div className="sbase">
      <div className="sbase-l">
        <span className={`sdot ${captureReady ? "ready" : "arming"}`} />
      </div>
      {waveform}
      <div className="sbase-r">
        {showTimer && <span className="stimer">{fmtTime(elapsed)}</span>}
        {meetingBtn}
        {showCancel && cancelBtn}
      </div>
    </div>
  );

  // spinner (left) | label (center) | cancel (right) — same 3-zone grid as the
  // listening row, so the label is centered.
  const workingRow = (label: string, showCancel: boolean) => (
    <div className="sbase">
      <div className="sbase-l">
        <span className="sspinner" />
      </div>
      <span className="swork-label">{label}</span>
      <div className="sbase-r">{showCancel && cancelBtn}</div>
    </div>
  );

  // ---- Live overlay: a pill that sculpts open into a panel ----
  if (state === "streaming") {
    const hasText =
      streamText.committed.length > 0 || streamText.tentative.length > 0;
    const working = phase === "working";
    // While listening, a slow model load opens the panel with a notice where the
    // text would be, so speech that isn't transcribing yet doesn't look ignored.
    const loadingNotice = modelLoadSlow && !working && !hasText;
    // Keep the panel open whenever there's text — even while finalizing — so the
    // transcript stays put under a working spinner instead of collapsing and
    // squishing the text mid-stream. Only fall back to the small working pill
    // when there was no text to preserve.
    const open = hasText || loadingNotice || (loadNoticeShown && !working);
    const collapsed = working && !hasText;

    return (
      <div dir={direction} className={`ov-stage ${position}`}>
        <div
          key={session}
          className={`scard ${open ? "open" : ""} ${collapsed ? "working" : ""} ${
            isVisible ? "" : "leaving"
          }`}
        >
          <div className="stext">
            <div className="stext-clip">
              <div
                className={`stext-cap ${overflowing ? "overflowing" : ""}`}
                ref={capRef}
                onScroll={handleStreamScroll}
              >
                <p>
                  {loadingNotice && (
                    <span className="sloading">
                      {t("overlay.loadingModel")}
                    </span>
                  )}
                  <span className="committed">
                    {streamText.committed ? streamText.committed + " " : ""}
                  </span>
                  <span className="tentative">{streamText.tentative}</span>
                  {/* Drop the blinking caret once finalizing — it's no longer
                      capturing, and a static spinner conveys the work. */}
                  {!working && !loadingNotice && <span className="scaret" />}
                </p>
              </div>
            </div>
          </div>
          {working
            ? workingRow(
                workKind === "polishing"
                  ? t("overlay.processing")
                  : transcribingLabel,
                true,
              )
            : listeningRow(open, true)}
        </div>
      </div>
    );
  }

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

  const stageClass = `ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`;

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
                <svg viewBox="0 0 16 16" aria-hidden="true">
                  <rect
                    x="4"
                    y="4"
                    width="8"
                    height="8"
                    rx="1.5"
                    fill="currentColor"
                  />
                </svg>
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
                {t("overlay.meeting.stopRecorded", { time: fmtTime(elapsed) })}
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
            <button
              className="sopt primary"
              onClick={() => commands.stopMeeting("transcribe")}
            >
              {checkIcon}
              {t("overlay.meeting.stopTranscribe")}
            </button>
            <button
              className="sopt"
              onClick={() => commands.stopMeeting("summarize")}
            >
              {sparkIcon}
              {t("overlay.meeting.stopSummarize")}
            </button>
            <button
              className="sopt plain"
              title={t("overlay.meeting.stopNothingHint")}
              onClick={() => commands.stopMeeting("nothing")}
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

  // ---- Post-meeting card: title + note, and a row of actions. Stays up until
  // the user picks one.
  if (state === "done" || state === "summarized") {
    const done = state === "done";
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
          <div className="sresult-actions">
            {done ? (
              <button
                className="sbtn"
                onClick={() => commands.summarizeMeeting(card.name)}
              >
                {sparkIcon}
                {t("overlay.meeting.summarize")}
              </button>
            ) : (
              <button className="sbtn" onClick={copySummary}>
                {copied ? checkIcon : copyIcon}
                {copied
                  ? t("overlay.meeting.copied")
                  : t("overlay.meeting.copySummary")}
              </button>
            )}
            <button
              className="sbtn ghost"
              onClick={() => commands.openMeetingFolder(card.name)}
            >
              {folderIcon}
              {t("overlay.meeting.openFolder")}
            </button>
            <button
              className="sbtn plain"
              onClick={() => commands.dismissMeetingInvite()}
            >
              {done ? t("overlay.meeting.later") : t("overlay.meeting.dismiss")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ---- Minimal overlay: exactly one row at a time — waveform (recording), or a
  // spinner + label (transcribing / processing). Never both. The pill animates its
  // width between them; the cancel button is in both rows so it stays put.
  const working = state === "transcribing" || state === "processing";
  const workLabel =
    state === "processing" ? t("overlay.processing") : transcribingLabel;

  return (
    <div
      dir={direction}
      className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
    >
      <div
        className={`scard compact ${working && isVisible ? "cworking" : ""}`}
      >
        {working ? workingRow(workLabel, true) : listeningRow(false, true)}
      </div>
    </div>
  );
};

export default RecordingOverlay;
