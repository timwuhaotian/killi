import { createServer } from "node:http";
import { loadSettings } from "./config/settings.ts";
import { negotiateLocale } from "./i18n/negotiate.ts";
import { SUPPORTED_LOCALES } from "./i18n/supported.ts";

const GREETINGS: Record<string, string> = {
  "en-US": "Hello",
  "en-GB": "Hello",
  "pt-BR": "Olá",
  "es-MX": "Hola",
};

const settings = loadSettings();

createServer((req, res) => {
  const locale = negotiateLocale(req.headers["accept-language"], SUPPORTED_LOCALES, settings);
  res.setHeader("content-language", locale);
  res.end(`${GREETINGS[locale]}!\n`);
}).listen(settings.port);
