import { Channel, invoke } from '@tauri-apps/api/core';
import type {
  CloudModel,
  HistoryEntry,
  ModelInfo,
  ResearchResponse,
  SettingsStatus,
  StreamEvent,
} from './types';

export function getSettingsStatus() {
  return invoke<SettingsStatus>('get_settings_status');
}

export function setup(apiKey: string) {
  return invoke<CloudModel[]>('setup', { apiKey });
}

export function setupLocal(baseUrl: string) {
  return invoke<ModelInfo[]>('setup_local', { baseUrl });
}

export function saveApiKey(apiKey: string) {
  return invoke<CloudModel[]>('save_api_key', { apiKey });
}

export function saveModel(model: string, visionModel: string | null, mode: string) {
  return invoke<void>('save_model', { model, visionModel, mode });
}

export function saveLocal(baseUrl: string, enabled: boolean) {
  return invoke<ModelInfo[]>('save_local', { baseUrl, enabled });
}

export function listModels() {
  return invoke<CloudModel[]>('list_models');
}

export function listLocalModels() {
  return invoke<ModelInfo[]>('list_local_models');
}

export function listAllModels() {
  return invoke<ModelInfo[]>('list_all_models');
}

export function research(
  query: string,
  history: HistoryEntry[],
  images: string[],
  onEvent: (event: StreamEvent) => void,
) {
  const channel = new Channel<StreamEvent>();
  channel.onmessage = onEvent;
  return invoke<ResearchResponse>('research', { query, history, images, onEvent: channel });
}

export function resetSettings() {
  return invoke<void>('reset_settings');
}