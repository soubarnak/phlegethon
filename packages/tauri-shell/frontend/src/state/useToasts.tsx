import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { ToastNotification } from "@carbon/react";
import { errorToString, parseCoreError, errorHint } from "@/lib/errors";

export type ToastKind = "error" | "info" | "success" | "warning";

export interface Toast {
  id: number;
  kind: ToastKind;
  title: string;
  subtitle?: string;
  caption?: string;
  timeout?: number;
}

interface ToastApi {
  push(toast: Omit<Toast, "id">): void;
  error(title: string, err?: unknown): void;
  info(title: string, subtitle?: string): void;
  success(title: string, subtitle?: string): void;
  warning(title: string, subtitle?: string): void;
}

/** Stacked toasts beyond this many drop the oldest, so the stack never covers much of a narrow window (UI-3). */
const MAX_TOASTS = 3;

const ToastContext = createContext<ToastApi | null>(null);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(1);

  const dismiss = useCallback((id: number) => {
    setToasts((list) => list.filter((t) => t.id !== id));
  }, []);

  const push = useCallback((toast: Omit<Toast, "id">) => {
    const id = nextId.current++;
    setToasts((list) => [...list.slice(-(MAX_TOASTS - 1)), { ...toast, id }]);
  }, []);

  const api = useMemo<ToastApi>(
    () => ({
      push,
      error(title, err) {
        const parsed = parseCoreError(err);
        const hint = errorHint(parsed.code);
        push({
          kind: "error",
          title,
          subtitle: parsed.message || (err == null ? undefined : errorToString(err)),
          caption: hint ?? undefined,
          timeout: 12000,
        });
      },
      info(title, subtitle) {
        push({ kind: "info", title, subtitle, timeout: 6000 });
      },
      success(title, subtitle) {
        push({ kind: "success", title, subtitle, timeout: 5000 });
      },
      warning(title, subtitle) {
        push({ kind: "warning", title, subtitle, timeout: 9000 });
      },
    }),
    [push],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      <div className="ph-toasts" aria-live="polite">
        {toasts.map((toast) => (
          <ToastNotification
            key={toast.id}
            kind={toast.kind}
            title={toast.title}
            subtitle={toast.subtitle}
            caption={toast.caption}
            timeout={toast.timeout}
            lowContrast
            onClose={() => {
              dismiss(toast.id);
              return false;
            }}
            onCloseButtonClick={() => dismiss(toast.id)}
          />
        ))}
      </div>
    </ToastContext.Provider>
  );
}

export function useToasts(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error("useToasts must be used within ToastProvider");
  }
  return ctx;
}
