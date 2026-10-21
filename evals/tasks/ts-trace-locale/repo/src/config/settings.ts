import { DEFAULTS } from "./defaults.ts";
import { DEFAULT_REGION, REGION_LOCALES, type Region } from "./region.ts";

export interface Settings {
  locale: string;
  timezone: string;
  port: number;
}

export function loadSettings(env: Record<string, string | undefined> = process.env): Settings {
  const region = (env.APP_REGION as Region | undefined) ?? DEFAULT_REGION;
  return {
    ...DEFAULTS,
    locale: env.APP_LOCALE ?? REGION_LOCALES[region] ?? DEFAULTS.locale,
    port: Number(env.PORT ?? DEFAULTS.port),
  };
}
