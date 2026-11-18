import { useEffect } from "react";
import { commands } from "@/bindings";

// Slack (CSS px) around the card where the overlay still takes clicks.
const HIT_REGION_PAD = 6;

/**
 * The overlay window is a transparent rectangle bigger than the card, so tell
 * the backend to take clicks only over the card; the rest of the window lets
 * them through to whatever is below. While hidden, nothing takes clicks.
 * `deps` are what can move or resize the card.
 */
export function useOverlayHitRegion(isVisible: boolean, deps: unknown[]) {
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
  }, [isVisible, ...deps]);
}
