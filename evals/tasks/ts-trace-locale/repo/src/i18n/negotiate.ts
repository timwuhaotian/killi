import type { Settings } from "../config/settings.ts";
import { resolveFallbackLocale } from "./fallback.ts";

/** Picks the best supported locale for an Accept-Language header. */
export function negotiateLocale(header: string | undefined, supported: string[], settings: Settings): string {
  if (!header) {
    return resolveFallbackLocale(settings, supported);
  }
  const wanted = header
    .split(",")
    .map((part) => {
      const [tag, q] = part.trim().split(";q=");
      return { tag, q: q === undefined ? 1 : Number(q) };
    })
    .sort((a, b) => b.q - a.q);
  for (const { tag } of wanted) {
    const exact = supported.find((s) => s.toLowerCase() === tag.toLowerCase());
    if (exact) return exact;
    const sameLanguage = supported.find((s) => s.split("-")[0] === tag.split("-")[0]);
    if (sameLanguage) return sameLanguage;
  }
  return resolveFallbackLocale(settings, supported);
}
