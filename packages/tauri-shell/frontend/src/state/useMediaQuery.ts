import { useSyncExternalStore } from "react";

/** Window width below which the queue switches to its compact layout. Keep in step with `$ph-bp-md` in `styles/index.scss`. */
export const NARROW_QUERY = "(max-width: 699.98px)";

/** Tracks a CSS media query; `false` where `matchMedia` is unavailable. */
export function useMediaQuery(query: string): boolean {
  return useSyncExternalStore(
    (notify) => {
      const list = window.matchMedia?.(query);
      list?.addEventListener?.("change", notify);
      return () => list?.removeEventListener?.("change", notify);
    },
    () => window.matchMedia?.(query).matches ?? false,
    () => false,
  );
}
