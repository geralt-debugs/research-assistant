export interface CloudModel {
  name: string;
  parameterSize: string;
  capabilities: string[];
}

export type Endpoint = 'cloud' | 'local';

export interface ModelInfo {
  endpoint: Endpoint;
  name: string;
  parameterSize: string;
  capabilities: string[];
}

export function encodeModelId(endpoint: Endpoint, name: string): string {
  return `${endpoint}:${name}`;
}

export function decodeModelId(model: string): { endpoint: Endpoint; name: string } {
  if (model.startsWith('local:')) return { endpoint: 'local', name: model.slice('local:'.length) };
  if (model.startsWith('cloud:')) return { endpoint: 'cloud', name: model.slice('cloud:'.length) };
  return { endpoint: 'cloud', name: model };
}

export function modelLabel(model: string): string {
  const { endpoint, name } = decodeModelId(model);
  const clean = name.replace(/-cloud$/, '');
  return endpoint === 'local' ? `${clean} (local)` : clean;
}

export interface ResearchSource {
  title: string;
  url: string;
  content: string;
}

export interface ResearchResponse {
  message: string;
  sources: ResearchSource[];
  model: string;
  thinking: string;
}

export type ResearchPhase = 'planning' | 'searching' | 'writing' | null;

export type StreamEvent =
  | { kind: 'phase'; phase: string }
  | { kind: 'thinkingDelta'; delta: string }
  | { kind: 'answerDelta'; delta: string }
  | { kind: 'done'; data: ResearchResponse };

export interface HistoryEntry {
  role: string;
  content: string;
}

export interface SettingsStatus {
  configured: boolean;
  model: string;
  visionModel: string | null;
  mode: string;
  localBaseUrl: string;
  localEnabled: boolean;
  hasApiKey: boolean;
}

export interface ResearchMessage {
  query: string;
  answer: string;
  thinking: string;
  sources: ResearchSource[];
  images: string[];
  modelName: string;
  createdAt: number;
}

export interface ResearchSession {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messages: ResearchMessage[];
  modelName: string;
}
