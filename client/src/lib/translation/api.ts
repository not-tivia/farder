// client/src/lib/translation/api.ts

import { invoke } from "@tauri-apps/api/core";
import type {
  AvailablePair,
  LangPair,
  LocalModel,
  ModelPaths,
  TranslationSettings,
} from "./types";

export async function getTranslationSettings(): Promise<TranslationSettings> {
  return invoke<TranslationSettings>("get_translation_settings");
}

export async function setTranslationSettings(settings: TranslationSettings): Promise<void> {
  return invoke<void>("set_translation_settings", { settings });
}

export async function listAvailablePairs(): Promise<AvailablePair[]> {
  return invoke<AvailablePair[]>("list_available_pairs");
}

export async function listLocalModels(): Promise<LocalModel[]> {
  return invoke<LocalModel[]>("list_local_models");
}

export async function downloadModel(pair: LangPair): Promise<LocalModel> {
  return invoke<LocalModel>("download_model", { pair });
}

export async function deleteModel(pair: LangPair): Promise<void> {
  return invoke<void>("delete_model", { pair });
}

export async function getModelPaths(pair: LangPair): Promise<ModelPaths> {
  return invoke<ModelPaths>("get_model_paths", { pair });
}
