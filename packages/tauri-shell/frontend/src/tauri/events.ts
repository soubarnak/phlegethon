/** Tauri event subscriptions (core-event, companion-intent). */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CompanionIntent, CoreEvent } from "./types";

export function onCoreEvent(handler: (event: CoreEvent) => void): Promise<UnlistenFn> {
  return listen<CoreEvent>("core-event", (e) => handler(e.payload));
}

export function onCompanionIntent(handler: (intent: CompanionIntent) => void): Promise<UnlistenFn> {
  return listen<CompanionIntent>("companion-intent", (e) => handler(e.payload));
}
