/** Pure selection logic for the Probe-then-Confirm sheet. */
import type { MediaExtrasOptions, MediaItem, ProbeSelection, Quality } from "@/tauri/types";

export type QualityByItem = Record<string, string>;

/** Everything is selected by default except items already in the Download Archive. */
export function defaultSelectedIds(items: MediaItem[]): Set<string> {
  return new Set(items.filter((i) => !i.archived).map((i) => i.id));
}

/** First quality id of an item (fallback when the policy picks nothing). */
export function firstQualityId(item: MediaItem): string | null {
  return item.qualities[0]?.id ?? null;
}

export function selectedItems(items: MediaItem[], selected: Set<string>): MediaItem[] {
  return items.filter((i) => selected.has(i.id));
}

/** The chosen quality of an item, falling back to its first one. */
export function chosenQuality(item: MediaItem, qualityByItem: QualityByItem): Quality | null {
  const wanted = qualityByItem[item.id];
  return (
    item.qualities.find((q) => q.id === wanted) ??
    item.qualities.find((q) => q.id === firstQualityId(item)) ??
    null
  );
}

/** Build the confirm payload for the selected items. Items with no quality are skipped. */
export function buildSelection(
  url: string,
  items: MediaItem[],
  selected: Set<string>,
  qualityByItem: QualityByItem,
  mediaExtras: MediaExtrasOptions | null,
): ProbeSelection {
  const picks = selectedItems(items, selected).flatMap((item) => {
    const q = chosenQuality(item, qualityByItem);
    return q ? [{ item_id: item.id, quality_id: q.id }] : [];
  });
  return { url, items: picks, media_extras: mediaExtras };
}

/** Sum of known sizes of the chosen qualities; `unknown` counts items without a size. */
export function totalSelectedBytes(
  items: MediaItem[],
  selected: Set<string>,
  qualityByItem: QualityByItem,
): { bytes: number; unknown: number } {
  let bytes = 0;
  let unknown = 0;
  for (const item of selectedItems(items, selected)) {
    const q = chosenQuality(item, qualityByItem);
    if (q?.size_bytes != null && q.size_bytes > 0) bytes += q.size_bytes;
    else unknown += 1;
  }
  return { bytes, unknown };
}

/** Distinct quality ids across all items, highest resolution first. */
export function commonQualityOptions(items: MediaItem[]): Array<{ id: string; label: string }> {
  const seen = new Map<string, string>();
  for (const item of items) {
    for (const q of item.qualities) {
      // Sniffed variants carry a URL after `@`; compare by the quality part only.
      const id = q.id.split("@")[0];
      if (!seen.has(id)) seen.set(id, q.label.split(" (")[0]);
    }
  }
  const height = (id: string) => {
    const m = /^(\d+)p$/.exec(id);
    return m ? Number(m[1]) : id === "best" ? 100000 : -1;
  };
  return Array.from(seen, ([id, label]) => ({ id, label })).sort((a, b) => height(b.id) - height(a.id));
}

/**
 * Apply one quality id to every selected item that offers it; others keep their choice.
 * Returns the new map (does not mutate).
 */
export function applyQualityToAll(
  items: MediaItem[],
  selected: Set<string>,
  qualityId: string,
  current: QualityByItem,
): QualityByItem {
  const next: QualityByItem = { ...current };
  for (const item of selectedItems(items, selected)) {
    const match = item.qualities.find((q) => q.id.split("@")[0] === qualityId);
    if (match) next[item.id] = match.id;
  }
  return next;
}

export function invertSelection(items: MediaItem[], selected: Set<string>): Set<string> {
  return new Set(items.filter((i) => !selected.has(i.id)).map((i) => i.id));
}

/** Keep the user's pick when an enrich replaces the quality list. */
export function remapQualityAfterEnrich(
  previousId: string | undefined,
  newQualities: Quality[],
): string | null {
  if (previousId && newQualities.some((q) => q.id === previousId)) return previousId;
  // Presets like `720p` usually survive; otherwise take the best available.
  return newQualities[0]?.id ?? null;
}
