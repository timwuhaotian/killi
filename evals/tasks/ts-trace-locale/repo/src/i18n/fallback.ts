import type { Settings } from "../config/settings.ts";

/** The locale to use when the request doesn't say which one it wants. */
export function resolveFallbackLocale(settings: Settings, supported: string[]): string {
  if (supported.includes(settings.locale)) {
    return settings.locale;
  }
  return supported[0];
}
