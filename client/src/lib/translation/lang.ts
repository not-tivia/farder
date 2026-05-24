// client/src/lib/translation/lang.ts

// Minimal map covering the languages we expect to see — the most common chat languages.
// Bergamot uses ISO 639-1 codes; franc returns 639-3. Map between them here.

export const ISO_3_TO_1: Record<string, string> = {
  eng: "en", spa: "es", zho: "zh", cmn: "zh",
  fra: "fr", deu: "de", por: "pt", rus: "ru",
  ita: "it", jpn: "ja", kor: "ko", ara: "ar",
  nld: "nl", pol: "pl", ukr: "uk", tur: "tr",
  vie: "vi", tha: "th", hin: "hi", ben: "bn",
  fas: "fa", swe: "sv", nor: "no", dan: "da",
  fin: "fi", ces: "cs", hun: "hu", ron: "ro",
  ell: "el", heb: "he", ind: "id", msa: "ms",
};

export const ISO_1_TO_3: Record<string, string> = Object.fromEntries(
  Object.entries(ISO_3_TO_1).map(([three, one]) => [one, three])
);

export const DISPLAY_NAMES: Record<string, string> = {
  en: "English", es: "Spanish", zh: "Chinese", fr: "French",
  de: "German", pt: "Portuguese", ru: "Russian", it: "Italian",
  ja: "Japanese", ko: "Korean", ar: "Arabic", nl: "Dutch",
  pl: "Polish", uk: "Ukrainian", tr: "Turkish", vi: "Vietnamese",
  th: "Thai", hi: "Hindi", bn: "Bengali", fa: "Persian",
  sv: "Swedish", no: "Norwegian", da: "Danish", fi: "Finnish",
  cs: "Czech", hu: "Hungarian", ro: "Romanian", el: "Greek",
  he: "Hebrew", id: "Indonesian", ms: "Malay",
};

export function displayName(iso1: string): string {
  return DISPLAY_NAMES[iso1] ?? iso1.toUpperCase();
}

export function iso3to1(iso3: string): string | null {
  return ISO_3_TO_1[iso3] ?? null;
}
