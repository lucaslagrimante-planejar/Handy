import React, { useState } from "react";
import { ChevronDown } from "lucide-react";

const STORAGE_PREFIX = "history-section-open:";

const readOpen = (key: string, fallback: boolean) => {
  try {
    const stored = localStorage.getItem(STORAGE_PREFIX + key);
    return stored === null ? fallback : stored === "1";
  } catch {
    return fallback;
  }
};

/**
 * A history list with a header that collapses it. The open state is
 * remembered per section, so a long list stays out of the way.
 */
export const CollapsibleSection: React.FC<{
  id: string;
  title: string;
  count?: number;
  defaultOpen?: boolean;
  actions?: React.ReactNode;
  children: React.ReactNode;
}> = ({ id, title, count, defaultOpen = true, actions, children }) => {
  const [open, setOpen] = useState(() => readOpen(id, defaultOpen));

  const toggle = () => {
    setOpen((prev) => {
      try {
        localStorage.setItem(STORAGE_PREFIX + id, prev ? "0" : "1");
      } catch {
        // Not persisted; the section still toggles for this session.
      }
      return !prev;
    });
  };

  return (
    <div className="space-y-2">
      <div className="px-4 flex items-center justify-between gap-2">
        <button
          type="button"
          onClick={toggle}
          aria-expanded={open}
          className="flex items-center gap-1.5 cursor-pointer text-mid-gray hover:text-text transition-colors"
        >
          <ChevronDown
            className={`w-4 h-4 transition-transform ${open ? "" : "-rotate-90"}`}
          />
          <h2 className="text-xs font-medium uppercase tracking-wide">
            {title}
          </h2>
          {count !== undefined && (
            <span className="text-xs text-text/40 tabular-nums">({count})</span>
          )}
        </button>
        {actions}
      </div>
      {open && children}
    </div>
  );
};
