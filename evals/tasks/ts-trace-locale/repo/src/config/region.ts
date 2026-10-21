export type Region = "na" | "eu" | "latam";

export const DEFAULT_REGION: Region = "latam";

export const REGION_LOCALES: Record<Region, string> = {
  na: "en-US",
  eu: "en-GB",
  latam: "pt-BR",
};
