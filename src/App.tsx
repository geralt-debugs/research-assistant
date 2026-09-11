import { FormEvent, useEffect, useRef, useState } from 'react';
import {
  ArrowLeft,
  ArrowRight,
  BookOpenText,
  Brain,
  Check,
  ChevronDown,
  Clock3,
  Cpu,
  ExternalLink,
  Eye,
  Gauge,
  History,
  Home,
  ImagePlus,
  LoaderCircle,
  MessageSquareText,
  Plus,
  Search,
  Server,
  Settings,
  SlidersHorizontal,
  Sparkles,
  Wrench,
  X,
  Zap,
} from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { openUrl } from '@tauri-apps/plugin-opener';
import { getSettingsStatus, listAllModels, research, resetSettings, saveApiKey, saveModel, saveLocal, setup, setupLocal } from './api';
import type {
  CloudModel,
  ModelInfo,
  ResearchMessage,
  ResearchPhase,
  ResearchSession,
  ResearchSource,
  SettingsStatus,
} from './types';
import { decodeModelId, encodeModelId, modelLabel } from './types';

const HISTORY_KEY = 'research-assistant-history-v1';
const MAX_IMAGES = 3;
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const MODES: { value: string; label: string }[] = [
  { value: 'speed', label: 'Speed' },
  { value: 'balanced', label: 'Balanced' },
  { value: 'quality', label: 'Quality' },
];

const emptyStatus: SettingsStatus = {
  configured: false,
  model: '',
  visionModel: null,
  mode: '',
  localBaseUrl: '',
  localEnabled: false,
  hasApiKey: false,
};

function App() {
  const [booting, setBooting] = useState(true);
  const [needsSetup, setNeedsSetup] = useState(false);
  const [status, setStatus] = useState<SettingsStatus>(emptyStatus);
  const [models, setModels] = useState<CloudModel[]>([]);
  const [allModels, setAllModels] = useState<ModelInfo[]>([]);
  const [sessions, setSessions] = useState<ResearchSession[]>(loadHistory);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [view, setView] = useState<'research' | 'history'>('research');
  const [showSettings, setShowSettings] = useState(false);
  const [query, setQuery] = useState('');
  const [images, setImages] = useState<string[]>([]);
  const [pendingQuery, setPendingQuery] = useState('');
  const [searching, setSearching] = useState(false);
  const [phase, setPhase] = useState<ResearchPhase>(null);
  const [liveAnswer, setLiveAnswer] = useState('');
  const [liveThinking, setLiveThinking] = useState('');
  const [error, setError] = useState('');

  const currentSession = sessions.find((session) => session.id === currentId) ?? null;

  useEffect(() => {
    let active = true;
    getSettingsStatus()
      .then(async (status) => {
        if (!active) return;
        setStatus(status);
        if (!status.configured) {
          setNeedsSetup(true);
          return;
        }
        try {
          const available = await listAllModels();
          if (active) setAllModels(available);
          if (status.hasApiKey) {
            const cloud = available
              .filter((m) => m.endpoint === 'cloud')
              .map((m) => ({ name: m.name, parameterSize: m.parameterSize, capabilities: m.capabilities }));
            if (active) setModels(cloud);
          }
        } catch (reason) {
          if (active) setError(errorMessage(reason));
        }
      })
      .catch((reason) => {
        if (active) {
          setError(errorMessage(reason));
          setNeedsSetup(true);
        }
      })
      .finally(() => {
        if (active) setBooting(false);
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify(sessions.slice(0, 50)));
    } catch {
      // Storage full (large image attachments): keep the session in memory
      // and drop older ones until it fits.
      setSessions((items) => {
        const trimmed = items.slice(0, Math.max(1, items.length - 1));
        if (trimmed.length === items.length) return items;
        try {
          localStorage.setItem(HISTORY_KEY, JSON.stringify(trimmed));
        } catch {
          // Still too large; skip persisting this round.
        }
        return trimmed;
      });
    }
  }, [sessions]);

  async function finishSetup(nextStatus: SettingsStatus, available: ModelInfo[]) {
    setStatus(nextStatus);
    setAllModels(available);
    setModels(available.filter((m) => m.endpoint === 'cloud').map(toCloudModel));
    setNeedsSetup(false);
    setError('');
  }

  function toCloudModel(model: ModelInfo): CloudModel {
    return { name: model.name, parameterSize: model.parameterSize, capabilities: model.capabilities };
  }

  async function updateModelAndMode(model: string, visionModel: string | null, mode: string) {
    try {
      await saveModel(model, visionModel, mode);
      setStatus({ ...status, configured: true, model, visionModel, mode });
    } catch (reason) {
      setError(errorMessage(reason));
    }
  }

  async function handleReset() {
    try {
      await resetSettings();
      setStatus(emptyStatus);
      setModels([]);
      setAllModels([]);
      setNeedsSetup(true);
      setShowSettings(false);
      setError('');
      newResearch();
    } catch (reason) {
      setError(errorMessage(reason));
    }
  }

  function newResearch() {
    setCurrentId(null);
    setView('research');
    setQuery('');
    setImages([]);
    setError('');
  }

  function addImages(incoming: string[]) {
    const room = MAX_IMAGES - images.length;
    if (incoming.length > room) {
      setError(`Attach at most ${MAX_IMAGES} images per question.`);
    }
    if (room <= 0) return;
    setImages((current) => [...current, ...incoming].slice(0, MAX_IMAGES));
  }

  async function submitSearch(value: string) {
    const cleanQuery = value.trim();
    if ((!cleanQuery && images.length === 0) || searching) return;
    if (!status.model) {
      setError('Choose a model in Settings.');
      setShowSettings(true);
      return;
    }

    const attached = images;
    setSearching(true);
    setPendingQuery(cleanQuery || 'Describe the attached images');
    setQuery('');
    setImages([]);
    setError('');
    setPhase('planning');
    setLiveAnswer('');
    setLiveThinking('');

    const history =
      currentSession?.messages.flatMap((message) => [
        { role: 'user', content: message.query },
        { role: 'assistant', content: message.answer },
      ]) ?? [];

    try {
      const effectiveQuery = cleanQuery || 'Describe the attached images';
      const response = await research(effectiveQuery, history, attached, (event) => {
        switch (event.kind) {
          case 'phase':
            setPhase(event.phase as ResearchPhase);
            break;
          case 'thinkingDelta':
            setLiveThinking((current) => current + event.delta);
            break;
          case 'answerDelta':
            setLiveAnswer((current) => current + event.delta);
            break;
          case 'done':
            break;
        }
      });
      const now = Date.now();
      const message: ResearchMessage = {
        query: effectiveQuery,
        answer: response.message,
        thinking: response.thinking,
        sources: response.sources,
        images: attached,
        modelName: response.model || status.model,
        createdAt: now,
      };

      if (currentSession) {
        setSessions((items) =>
          items
            .map((session) =>
              session.id === currentSession.id
                ? { ...session, updatedAt: now, messages: [...session.messages, message] }
                : session,
            )
            .sort((a, b) => b.updatedAt - a.updatedAt),
        );
      } else {
        const session: ResearchSession = {
          id: crypto.randomUUID(),
          title: cleanQuery || 'Image analysis',
          createdAt: now,
          updatedAt: now,
          messages: [message],
          modelName: response.model || status.model,
        };
        setSessions((items) => [session, ...items]);
        setCurrentId(session.id);
      }
    } catch (reason) {
      setError(errorMessage(reason));
      setQuery(cleanQuery);
      setImages(attached);
    } finally {
      setSearching(false);
      setPendingQuery('');
      setPhase(null);
      setLiveAnswer('');
      setLiveThinking('');
    }
  }

  if (booting) return <BootScreen />;
  if (needsSetup) {
    return <Onboarding onComplete={finishSetup} globalError={error} />;
  }

  const composerProps: ComposerProps = {
    status,
    allModels,
    query,
    images,
    pendingQuery,
    searching,
    error,
    onQueryChange: setQuery,
    onImagesAdd: addImages,
    onImageRemove: (index) => setImages((current) => current.filter((_, i) => i !== index)),
    onSubmit: submitSearch,
    onModeChange: (mode) => updateModelAndMode(status.model, status.visionModel, mode),
    onModelChange: (model) => updateModelAndMode(model, status.visionModel, status.mode),
  };

  return (
    <div className="app-shell">
      <Navigation
        active={view}
        onNew={newResearch}
        onHome={() => setView('research')}
        onHistory={() => setView('history')}
        onSettings={() => setShowSettings(true)}
      />

      <main className="main-content">
        {view === 'history' ? (
          <HistoryView
            sessions={sessions}
            onOpen={(id) => {
              setCurrentId(id);
              setView('research');
            }}
            onDelete={(id) => {
              setSessions((items) => items.filter((session) => session.id !== id));
              if (currentId === id) setCurrentId(null);
            }}
          />
        ) : currentSession ? (
          <ResearchView
            session={currentSession}
            phase={phase}
            liveAnswer={liveAnswer}
            liveThinking={liveThinking}
            onBack={newResearch}
            {...composerProps}
          />
        ) : (
          <HomeView
            phase={phase}
            liveAnswer={liveAnswer}
            liveThinking={liveThinking}
            onOpenSettings={() => setShowSettings(true)}
            {...composerProps}
          />
        )}
      </main>

      {showSettings && (
        <SettingsDialog
          status={status}
          models={models}
          allModels={allModels}
          onModels={setModels}
          onAllModels={setAllModels}
          onClose={() => setShowSettings(false)}
          onSave={(model, visionModel, mode) => {
            setShowSettings(false);
            setError('');
            void updateModelAndMode(model, visionModel, mode);
          }}
          onReset={handleReset}
        />
      )}
    </div>
  );
}

function BootScreen() {
  return (
    <div className="boot-screen">
      <BrandMark />
      <LoaderCircle className="spin" size={20} />
    </div>
  );
}

function BrandMark() {
  return (
    <div className="brand-mark" aria-label="Research Assistant">
      <span>R</span>
      <i />
    </div>
  );
}

function Navigation({
  active,
  onNew,
  onHome,
  onHistory,
  onSettings,
}: {
  active: 'research' | 'history';
  onNew: () => void;
  onHome: () => void;
  onHistory: () => void;
  onSettings: () => void;
}) {
  return (
    <aside className="navigation">
      <button className="new-button" onClick={onNew} aria-label="New research">
        <Plus size={19} />
      </button>
      <nav>
        <NavButton active={active === 'research'} icon={<Home />} label="Home" onClick={onHome} />
        <NavButton active={active === 'history'} icon={<History />} label="History" onClick={onHistory} />
      </nav>
      <button className="settings-button" onClick={onSettings} aria-label="Settings">
        <Settings size={19} />
      </button>
    </aside>
  );
}

function NavButton({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: React.ReactElement<{ size?: number }>;
  label: string;
  onClick: () => void;
}) {
  return (
    <button className={`nav-button ${active ? 'active' : ''}`} onClick={onClick}>
      <span>{icon}</span>
      <small>{label}</small>
    </button>
  );
}

function HomeView({
  phase,
  liveAnswer,
  liveThinking,
  onOpenSettings,
  ...composerProps
}: ComposerProps & {
  phase: ResearchPhase;
  liveAnswer: string;
  liveThinking: string;
  onOpenSettings: () => void;
}) {
  return (
    <section className="home-view">
      <button className="mobile-settings" onClick={onOpenSettings} aria-label="Settings">
        <Settings size={19} />
      </button>
      <div className="home-inner">
        <div className="home-heading">
          <span className="eyebrow">Research Assistant</span>
          <h1>Research begins here.</h1>
          <p>Ollama Cloud models with live web search. Your API key stays encrypted on this device.</p>
        </div>
        <Composer {...composerProps} large />
        {composerProps.searching && (
          <LiveResearch phase={phase} liveAnswer={liveAnswer} liveThinking={liveThinking} pendingQuery={composerProps.pendingQuery} />
        )}
        {!composerProps.searching && (
          <div className="shortcut-hint"><kbd>/</kbd><span>Focus search</span></div>
        )}
      </div>
    </section>
  );
}

function ResearchView({
  session,
  phase,
  liveAnswer,
  liveThinking,
  onBack,
  ...composerProps
}: ComposerProps & {
  session: ResearchSession;
  phase: ResearchPhase;
  liveAnswer: string;
  liveThinking: string;
  onBack: () => void;
}) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (composerProps.searching) endRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [composerProps.searching, liveAnswer]);

  return (
    <section className="research-view">
      <header className="research-header">
        <button onClick={onBack} aria-label="Back to new research"><ArrowLeft size={18} /></button>
        <div>
          <strong>{session.title}</strong>
          <span>{formatDate(session.updatedAt)}</span>
        </div>
      </header>
      <div className="answer-column">
        {session.messages.map((message, index) => (
          <article className="answer-block" key={`${message.createdAt}-${index}`}>
            <h1>{message.query}</h1>
            {message.images.length > 0 && (
              <div className="attached-strip">
                {message.images.map((image, imageIndex) => (
                  <img key={imageIndex} src={`data:image/jpeg;base64,${image}`} alt={`Attachment ${imageIndex + 1}`} />
                ))}
              </div>
            )}
            {message.sources.length > 0 && <SourcesGrid sources={message.sources} />}
            {message.thinking && <ThinkingBlock text={message.thinking} />}
            <div className="answer-heading"><Sparkles size={19} /><h2>Answer</h2></div>
            <div className="markdown-answer">
              <ReactMarkdown
                remarkPlugins={[remarkGfm]}
                components={{
                  a: ({ href, children }) => (
                    <a href={href} onClick={(event) => openExternal(event, href)}>{children}</a>
                  ),
                }}
              >
                {message.answer}
              </ReactMarkdown>
            </div>
            <div className="answer-meta"><Cpu size={13} />{modelLabel(message.modelName)}</div>
          </article>
        ))}
        {composerProps.searching && (
          <LiveResearch
            phase={phase}
            liveAnswer={liveAnswer}
            liveThinking={liveThinking}
            pendingQuery={composerProps.pendingQuery}
          />
        )}
        <div ref={endRef} />
      </div>
      <div className="follow-up-dock">
        <Composer {...composerProps} compact />
      </div>
    </section>
  );
}

interface ComposerProps {
  status: SettingsStatus;
  allModels: ModelInfo[];
  query: string;
  images: string[];
  pendingQuery: string;
  searching: boolean;
  error: string;
  onQueryChange: (value: string) => void;
  onImagesAdd: (images: string[]) => void;
  onImageRemove: (index: number) => void;
  onSubmit: (value: string) => void;
  onModeChange: (mode: string) => void;
  onModelChange: (model: string) => void;
  large?: boolean;
  compact?: boolean;
}

function Composer({
  status,
  allModels,
  query,
  images,
  searching,
  error,
  onQueryChange,
  onImagesAdd,
  onImageRemove,
  onSubmit,
  onModeChange,
  onModelChange,
  large,
  compact,
}: ComposerProps) {
  const textarea = useRef<HTMLTextAreaElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    function focusComposer(event: KeyboardEvent) {
      const element = document.activeElement;
      const isEditing = element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement;
      if (event.key === '/' && !isEditing) {
        event.preventDefault();
        textarea.current?.focus();
      }
    }
    document.addEventListener('keydown', focusComposer);
    if (large) textarea.current?.focus();
    return () => document.removeEventListener('keydown', focusComposer);
  }, [large]);

  function submit(event: FormEvent) {
    event.preventDefault();
    onSubmit(query);
  }

  async function handleFiles(fileList: FileList | null) {
    if (!fileList) return;
    const payloads: string[] = [];
    for (const file of Array.from(fileList)) {
      if (!file.type.startsWith('image/')) continue;
      if (file.size > MAX_IMAGE_BYTES) continue;
      const dataUrl = await fileToBase64(file);
      // Re-encode as a downscaled JPEG data URL; the base64 body must stay
      // under the Tauri IPC size limit or the request is rejected.
      const reencoded = await downscaleImage(dataUrl);
      payloads.push(reencoded.split(',', 2)[1] ?? reencoded);
    }
    if (payloads.length > 0) onImagesAdd(payloads);
    if (fileInput.current) fileInput.current.value = '';
  }

  return (
    <div className={`composer-wrap ${large ? 'large' : ''} ${compact ? 'compact' : ''}`}>
      <form className="composer" onSubmit={submit}>
        <textarea
          ref={textarea}
          value={query}
          rows={large ? 3 : 1}
          placeholder={compact ? 'Ask a follow-up...' : 'Ask anything...'}
          aria-label="Research question"
          disabled={searching}
          onChange={(event) => onQueryChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.shiftKey) {
              event.preventDefault();
              onSubmit(query);
            }
          }}
        />
        {images.length > 0 && (
          <div className="attached-strip">
            {images.map((image, index) => (
              <span className="attached-thumb" key={index}>
                <img src={`data:image/jpeg;base64,${image}`} alt={`Attachment ${index + 1}`} />
                <button type="button" onClick={() => onImageRemove(index)} aria-label="Remove attachment"><X size={12} /></button>
              </span>
            ))}
          </div>
        )}
        <div className="composer-toolbar">
          <div className="composer-controls">
            <label className="select-control mode-control">
              {status.mode === 'speed' ? <Zap /> : status.mode === 'quality' ? <Sparkles /> : <Gauge />}
              <select
                value={status.mode}
                onChange={(event) => onModeChange(event.target.value)}
                aria-label="Research mode"
              >
                {MODES.map((mode) => (
                  <option value={mode.value} key={mode.value}>{mode.label}</option>
                ))}
              </select>
              <ChevronDown />
            </label>
            <button
              type="button"
              className="attach-button"
              onClick={() => fileInput.current?.click()}
              disabled={searching || images.length >= MAX_IMAGES}
              aria-label="Attach images"
              title="Attach images (answered by a vision-capable cloud model)"
            >
              <ImagePlus size={15} />
            </button>
            <input
              ref={fileInput}
              type="file"
              accept="image/*"
              multiple
              hidden
              onChange={(event) => void handleFiles(event.target.files)}
            />
            <ModelChip model={status.model} models={allModels} onModelChange={onModelChange} />
          </div>
          <div className="composer-actions">
            <button
              type="submit"
              className="send-button"
              disabled={(!query.trim() && images.length === 0) || searching || !status.model}
              aria-label="Start research"
            >
              {searching ? <LoaderCircle className="spin" /> : <ArrowRight />}
            </button>
          </div>
        </div>
      </form>
      {error && <div className="inline-error">{error}</div>}
    </div>
  );
}

function ModelChip({ model, models, onModelChange }: { model: string; models: ModelInfo[]; onModelChange: (model: string) => void }) {
  if (models.length === 0) {
    if (!model) return null;
    return (
      <span className="model-chip" title={model}>
        <Cpu size={13} />
        <span>{modelLabel(model)}</span>
      </span>
    );
  }
  return (
    <span className="model-chip model-chip-select">
      <Cpu size={13} />
      <select
        value={model}
        onChange={(event) => onModelChange(event.target.value)}
        aria-label="Model"
      >
        {models.length === 0 && <option value={model}>{modelLabel(model)}</option>}
        {groupedModels(models).map((group) => (
          <optgroup key={group.endpoint} label={group.endpoint === 'cloud' ? 'Cloud' : 'Local'}>
            {group.models.map((m) => (
              <option value={encodeModelId(m.endpoint, m.name)} key={`${m.endpoint}-${m.name}`}>
                {m.name}{m.parameterSize ? ` · ${m.parameterSize}` : ''}
              </option>
            ))}
          </optgroup>
        ))}
        {model && !models.some((m) => encodeModelId(m.endpoint, m.name) === model) && (
          <option value={model}>{modelLabel(model)}</option>
        )}
      </select>
      <ChevronDown size={12} />
    </span>
  );
}

interface ModelGroup {
  endpoint: 'cloud' | 'local';
  models: ModelInfo[];
}

function groupedModels(models: ModelInfo[]): ModelGroup[] {
  const groups: ModelGroup[] = [];
  for (const endpoint of ['cloud', 'local'] as const) {
    const group = models.filter((m) => m.endpoint === endpoint);
    if (group.length > 0) groups.push({ endpoint, models: group });
  }
  return groups;
}

function SourcesGrid({ sources }: { sources: ResearchSource[] }) {
  return (
    <section className="sources-section">
      <div className="section-heading"><BookOpenText size={18} /><h2>Sources</h2><span>{sources.length}</span></div>
      <div className="sources-grid">
        {sources.slice(0, 6).map((source, index) => (
          <button key={`${source.url}-${index}`} onClick={() => source.url && void openUrl(source.url)}>
            <span className="source-number">{index + 1}</span>
            <strong>{source.title}</strong>
            <small>{domain(source.url)}</small>
            <ExternalLink size={13} />
          </button>
        ))}
      </div>
    </section>
  );
}

function HistoryView({
  sessions,
  onOpen,
  onDelete,
}: {
  sessions: ResearchSession[];
  onOpen: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  return (
    <section className="history-view">
      <header><span className="eyebrow">Local library</span><h1>Research history</h1><p>Your research is stored only on this device.</p></header>
      {sessions.length === 0 ? (
        <div className="empty-history"><MessageSquareText size={28} /><strong>No research yet</strong><span>Your completed searches will appear here.</span></div>
      ) : (
        <div className="history-list">
          {sessions.map((session) => (
            <div className="history-row" key={session.id}>
              <button onClick={() => onOpen(session.id)}>
                <span className="history-icon"><Search size={17} /></span>
                <span className="history-copy"><strong>{session.title}</strong><small><Clock3 size={12} />{formatDate(session.updatedAt)} · {session.messages.length} {session.messages.length === 1 ? 'question' : 'questions'}</small></span>
                <ArrowRight size={17} />
              </button>
              <button className="delete-history" onClick={() => onDelete(session.id)} aria-label={`Delete ${session.title}`}><X size={16} /></button>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function ModelSelect({
  models,
  value,
  visionOnly,
  allowNone,
  label,
  onChange,
}: {
  models: ModelInfo[];
  value: string;
  visionOnly?: boolean;
  allowNone?: boolean;
  label: string;
  onChange: (value: string) => void;
}) {
  const listed = visionOnly ? models.filter((m) => m.capabilities.includes('vision')) : models;
  return (
    <label className="model-picker">
      <Cpu size={16} />
      <select value={value} onChange={(event) => onChange(event.target.value)} aria-label={label}>
        {allowNone ? (
          <option value="">Auto (any vision model)</option>
        ) : (
          <option value="">Choose model</option>
        )}
        {groupedModels(listed).map((group) => (
          <optgroup key={group.endpoint} label={group.endpoint === 'cloud' ? 'Cloud' : 'Local'}>
            {group.models.map((model) => (
              <option value={encodeModelId(model.endpoint, model.name)} key={`${model.endpoint}-${model.name}`}>
                {model.name}{model.parameterSize ? ` · ${model.parameterSize}` : ''}
              </option>
            ))}
          </optgroup>
        ))}
      </select>
      <ChevronDown size={14} />
    </label>
  );
}

function CapabilityBadges({ model }: { model?: CloudModel }) {
  if (!model) return null;
  return (
    <span className="capability-badges">
      {model.capabilities.includes('vision') && <span className="badge"><Eye size={11} />vision</span>}
      {model.capabilities.includes('tools') && <span className="badge"><Wrench size={11} />tools</span>}
    </span>
  );
}

function Onboarding({
  onComplete,
  globalError,
}: {
  onComplete: (status: SettingsStatus, models: ModelInfo[]) => void;
  globalError: string;
}) {
  const [step, setStep] = useState<'connect' | 'model'>('connect');
  const [backend, setBackend] = useState<'cloud' | 'local' | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState('http://127.0.0.1:11434');
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [selectedModel, setSelectedModel] = useState('');
  const [mode, setMode] = useState('balanced');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(globalError);

  const selected = models.find((m) => encodeModelId(m.endpoint, m.name) === selectedModel);

  async function connectCloud(event: FormEvent) {
    event.preventDefault();
    const key = apiKey.trim();
    if (!key) {
      setError('Enter your Ollama API key.');
      return;
    }
    setBusy(true);
    setError('');
    try {
      const cloud = await setup(key);
      if (cloud.length === 0) throw new Error('No cloud chat models are available for this key.');
      setBackend('cloud');
      setModels(cloud.map((m) => ({ endpoint: 'cloud' as const, name: m.name, parameterSize: m.parameterSize, capabilities: m.capabilities })));
      setStep('model');
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  async function connectLocal(event: FormEvent) {
    event.preventDefault();
    const url = baseUrl.trim();
    if (!url) {
      setError('Enter your local Ollama address.');
      return;
    }
    setBusy(true);
    setError('');
    try {
      const local = await setupLocal(url);
      setBackend('local');
      setModels(local);
      setSelectedModel(encodeModelId('local', local[0].name));
      setStep('model');
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  async function complete(event: FormEvent) {
    event.preventDefault();
    if (!selectedModel || !selected) {
      setError('Choose a model to continue.');
      return;
    }
    setBusy(true);
    setError('');
    try {
      await saveModel(selectedModel, null, mode);
      onComplete(
        {
          configured: true,
          model: selectedModel,
          visionModel: null,
          mode,
          localBaseUrl: backend === 'local' ? baseUrl.trim() : '',
          localEnabled: backend === 'local',
          hasApiKey: backend === 'cloud',
        },
        models,
      );
    } catch (reason) {
      setError(errorMessage(reason));
      setBusy(false);
    }
  }

  return (
    <div className="onboarding">
      <div className="onboarding-glow" />
      <div className="onboarding-card">
        <BrandMark />
        <div className="step-marker"><span className="active" /><span className={step === 'model' ? 'active' : ''} /></div>
        {step === 'connect' ? (
          backend === null ? (
            <>
              <span className="eyebrow">Welcome to Research Assistant</span>
              <h1>Choose your Ollama.</h1>
              <p>Use Ollama Cloud with an API key, or a local Ollama server on this machine or your network.</p>
              <div className="backend-choice">
                <button type="button" onClick={() => setBackend('cloud')}>
                  <Sparkles size={18} />
                  <strong>Ollama Cloud</strong>
                  <small>Hosted models with live web search. Needs an API key.</small>
                </button>
                <button type="button" onClick={() => setBackend('local')}>
                  <Cpu size={18} />
                  <strong>Local Ollama</strong>
                  <small>Models on your own device or LAN. Private, no key needed.</small>
                </button>
              </div>
            </>
          ) : backend === 'cloud' ? (
            <form onSubmit={connectCloud}>
              <button type="button" className="back-link" onClick={() => setBackend(null)}><ArrowLeft />Back</button>
              <span className="eyebrow">Ollama Cloud</span>
              <h1>Connect Ollama Cloud.</h1>
              <p>Create an API key at ollama.com/settings/keys. The key is encrypted and stored only on this device.</p>
              <label className="field-label"><span>Ollama API key</span>
                <input
                  type="password"
                  value={apiKey}
                  autoCapitalize="none"
                  autoCorrect="off"
                  autoFocus
                  placeholder="Your Ollama API key"
                  onChange={(event) => setApiKey(event.target.value)}
                />
              </label>
              {error && <div className="form-error">{error}</div>}
              <button className="primary-button" disabled={busy || !apiKey.trim()}>
                {busy ? <LoaderCircle className="spin" /> : <>Connect <ArrowRight /></>}
              </button>
            </form>
          ) : (
            <form onSubmit={connectLocal}>
              <button type="button" className="back-link" onClick={() => setBackend(null)}><ArrowLeft />Back</button>
              <span className="eyebrow">Local Ollama</span>
              <h1>Connect your server.</h1>
              <p>Start Ollama with <code>OLLAMA_HOST=0.0.0.0</code> to allow LAN access. Models must already be pulled.</p>
              <label className="field-label"><span>Ollama server address</span>
                <input
                  type="text"
                  value={baseUrl}
                  autoCapitalize="none"
                  autoCorrect="off"
                  autoFocus
                  placeholder="http://127.0.0.1:11434"
                  onChange={(event) => setBaseUrl(event.target.value)}
                />
              </label>
              {error && <div className="form-error">{error}</div>}
              <button className="primary-button" disabled={busy || !baseUrl.trim()}>
                {busy ? <LoaderCircle className="spin" /> : <>Connect <ArrowRight /></>}
              </button>
            </form>
          )
        ) : (
          <form onSubmit={complete}>
            <span className="eyebrow">Connected</span>
            <h1>Choose your model.</h1>
            <p>{backend === 'local' ? 'Models run on your own Ollama server; nothing leaves your network.' : 'Models run in Ollama Cloud; nothing is downloaded to this device.'}</p>
            <div className="model-fields">
              <div>
                <span>Research model <CapabilityBadges model={selected && toBadgeModel(selected)} /></span>
                <small>Answers questions and writes responses</small>
                <ModelSelect models={models} value={selectedModel} label="Research model" onChange={setSelectedModel} />
              </div>
              <div>
                <span>Research mode</span>
                <small>Context depth and search breadth</small>
                <div className="mode-segments">
                  {MODES.map((m) => (
                    <button
                      type="button"
                      className={mode === m.value ? 'active' : ''}
                      onClick={() => setMode(m.value)}
                      key={m.value}
                    >
                      {m.label}
                    </button>
                  ))}
                </div>
              </div>
            </div>
            {error && <div className="form-error">{error}</div>}
            <button className="primary-button" disabled={busy || !selectedModel}>
              {busy ? <LoaderCircle className="spin" /> : <>Begin research <ArrowRight /></>}
            </button>
          </form>
        )}
        <div className="privacy-note"><Check size={13} />API key encrypted with a device-derived key</div>
      </div>
    </div>
  );
}

function toBadgeModel(model: ModelInfo): CloudModel {
  return { name: model.name, parameterSize: model.parameterSize, capabilities: model.capabilities };
}

function SettingsDialog({
  status,
  models,
  allModels,
  onModels,
  onAllModels,
  onClose,
  onSave,
  onReset,
}: {
  status: SettingsStatus;
  models: CloudModel[];
  allModels: ModelInfo[];
  onModels: (models: CloudModel[]) => void;
  onAllModels: (models: ModelInfo[]) => void;
  onClose: () => void;
  onSave: (model: string, visionModel: string | null, mode: string) => void;
  onReset: () => void;
}) {
  const [draftModel, setDraftModel] = useState(status.model);
  const [draftMode, setDraftMode] = useState(status.mode || 'balanced');
  const [draftBaseUrl, setDraftBaseUrl] = useState(status.localBaseUrl || 'http://127.0.0.1:11434');
  const [draftLocalEnabled, setDraftLocalEnabled] = useState(status.localEnabled);
  const [busy, setBusy] = useState(false);
  const [localBusy, setLocalBusy] = useState(false);
  const [error, setError] = useState('');
  const [resetting, setResetting] = useState(false);

  const selected = allModels.find((m) => encodeModelId(m.endpoint, m.name) === draftModel);

  useEffect(() => {
    function close(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose();
    }
    document.addEventListener('keydown', close);
    return () => document.removeEventListener('keydown', close);
  }, [onClose]);

  async function refresh() {
    setBusy(true);
    setError('');
    try {
      const available = await listAllModels();
      onAllModels(available);
      onModels(available.filter((m) => m.endpoint === 'cloud').map((m) => ({ name: m.name, parameterSize: m.parameterSize, capabilities: m.capabilities })));
      if (available.length > 0 && !available.some((m) => encodeModelId(m.endpoint, m.name) === draftModel)) {
        setDraftModel('');
      }
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  async function probeLocal() {
    setLocalBusy(true);
    setError('');
    try {
      const local = await saveLocal(draftBaseUrl.trim(), true);
      const merged = mergeModels(allModels.filter((m) => m.endpoint !== 'local'), local);
      onAllModels(merged);
      setDraftLocalEnabled(true);
      if (local.length > 0 && !allModels.some((m) => m.endpoint === 'local')) {
        setDraftModel((current) => current || encodeModelId('local', local[0].name));
      }
    } catch (reason) {
      setDraftLocalEnabled(false);
      setError(errorMessage(reason));
    } finally {
      setLocalBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!draftModel) {
      setError('Choose a model before saving.');
      return;
    }
    onSave(draftModel, null, draftMode);
  }

  return (
    <div className="dialog-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <form className="settings-dialog" onSubmit={submit}>
        <header><div><span className="eyebrow">Application</span><h2>Settings</h2></div><button type="button" onClick={onClose} aria-label="Close settings"><X /></button></header>
        <section>
          <div className="settings-section-title"><Cpu /><div><strong>Model</strong><span>Switch between cloud and local models</span></div></div>
          <label>
            <span>Active model <CapabilityBadges model={selected && toBadgeModel(selected)} /></span>
            <ModelSelect
              models={allModels.length > 0 ? allModels : models.map((m) => ({ endpoint: 'cloud' as const, ...m }))}
              value={draftModel}
              label="Research model"
              onChange={setDraftModel}
            />
          </label>
        </section>
        <section>
          <div className="settings-section-title"><Server /><div><strong>Local Ollama server</strong><span>Models on this machine or your network</span></div></div>
          <label className="field-label">
            <span>Server address</span>
            <input
              type="text"
              value={draftBaseUrl}
              autoCapitalize="none"
              autoCorrect="off"
              placeholder="http://127.0.0.1:11434"
              onChange={(event) => setDraftBaseUrl(event.target.value)}
            />
          </label>
          <button type="button" className="secondary-button" style={{ marginTop: 10 }} onClick={probeLocal} disabled={localBusy}>
            {localBusy ? <LoaderCircle className="spin" size={13} /> : draftLocalEnabled ? 'Reconnect & refresh models' : 'Connect & load models'}
          </button>
          {draftLocalEnabled && <div className="local-status"><Check size={13} />Connected — local models available in the model list</div>}
        </section>
        <section>
          <div className="settings-section-title"><SlidersHorizontal /><div><strong>Ollama Cloud</strong><span>Fetched live with your encrypted API key</span></div></div>
          <div className="cloud-key-row">
            <ApiKeyControl
              hasKey={status.hasApiKey}
              onConnect={async (key) => {
                const cloud = await saveApiKey(key);
                const cloudModels: ModelInfo[] = cloud.map((m) => ({ endpoint: 'cloud' as const, name: m.name, parameterSize: m.parameterSize, capabilities: m.capabilities }));
                onAllModels(mergeModels(allModels.filter((m) => m.endpoint !== 'cloud'), cloudModels));
                onModels(cloud);
              }}
            />
          </div>
          <button type="button" className="secondary-button" style={{ marginTop: 12 }} onClick={refresh} disabled={busy}>
            {busy ? <LoaderCircle className="spin" size={13} /> : 'Refresh model list'}
          </button>
        </section>
        <section>
          <div className="settings-section-title"><SlidersHorizontal /><div><strong>Research defaults</strong><span>Also adjustable from the search box</span></div></div>
          <div className="mode-segments">
            {MODES.map((m) => (
              <button type="button" className={draftMode === m.value ? 'active' : ''} onClick={() => setDraftMode(m.value)} key={m.value}>{m.label}</button>
            ))}
          </div>
        </section>
        <section>
          <div className="settings-section-title"><SlidersHorizontal /><div><strong>Danger zone</strong><span>Remove the API key and models from this device</span></div></div>
          <button
            type="button"
            className="secondary-button"
            style={{ color: 'var(--danger)', borderColor: 'rgba(247,129,102,.35)' }}
            disabled={resetting}
            onClick={async () => {
              setResetting(true);
              try {
                await onReset();
              } finally {
                setResetting(false);
              }
            }}
          >
            {resetting ? 'Resetting...' : 'Reset application data'}
          </button>
        </section>
        {error && <div className="form-error settings-error">{error}</div>}
        <footer><button type="button" className="secondary-button" onClick={onClose}>Cancel</button><button className="save-button" disabled={busy || !draftModel}>Save settings</button></footer>
      </form>
    </div>
  );
}

function mergeModels(current: ModelInfo[], incoming: ModelInfo[]): ModelInfo[] {
  const merged = [...current];
  for (const model of incoming) {
    const index = merged.findIndex((m) => m.endpoint === model.endpoint && m.name === model.name);
    if (index >= 0) merged[index] = model;
    else merged.push(model);
  }
  return merged;
}

function ApiKeyControl({ hasKey, onConnect }: { hasKey: boolean; onConnect: (key: string) => Promise<void> }) {
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function connect() {
    const trimmed = key.trim();
    if (!trimmed) return;
    setBusy(true);
    setError('');
    try {
      await onConnect(trimmed);
      setKey('');
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="api-key-control">
      <div className="key-input-row">
        <input
          type="password"
          value={key}
          autoCapitalize="none"
          autoCorrect="off"
          placeholder={hasKey ? 'API key saved — enter a new key to replace it' : 'Add your Ollama API key'}
          onChange={(event) => setKey(event.target.value)}
        />
        <button type="button" className="secondary-button" onClick={() => void connect()} disabled={busy || !key.trim()}>
          {busy ? <LoaderCircle className="spin" size={13} /> : 'Save key'}
        </button>
      </div>
      {error && <div className="form-error">{error}</div>}
    </div>
  );
}

function LiveResearch({
  phase,
  liveAnswer,
  liveThinking,
  pendingQuery,
}: {
  phase: ResearchPhase;
  liveAnswer: string;
  liveThinking: string;
  pendingQuery: string;
}) {
  return (
    <div className="live-research">
      <div className="researching-state">
        <LoaderCircle className="spin" size={20} />
        <div><strong>{phaseLabel(phase)}</strong><span>{pendingQuery}</span></div>
      </div>
      {liveThinking && <ThinkingBlock text={liveThinking} streaming />}
      {liveAnswer && (
        <div className="markdown-answer live-answer">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{liveAnswer}</ReactMarkdown>
        </div>
      )}
    </div>
  );
}

function ThinkingBlock({ text, streaming }: { text: string; streaming?: boolean }) {
  return (
    <details className={`thinking-block ${streaming ? 'streaming' : ''}`} open={streaming}>
      <summary><Brain size={14} /><span>Thinking</span>{streaming && <LoaderCircle className="spin" size={12} />}</summary>
      <pre>{text}</pre>
    </details>
  );
}

function phaseLabel(phase: ResearchPhase): string {
  switch (phase) {
    case 'planning': return 'Planning searches';
    case 'searching': return 'Searching the web';
    case 'writing': return 'Writing answer';
    default: return 'Researching';
  }
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error('Could not read the selected image.'));
    reader.onload = () => {
      const dataUrl = String(reader.result ?? '');
      resolve(dataUrl.split(',', 2)[1] ?? '');
    };
    reader.readAsDataURL(file);
  });
}

/// Re-encode an image data URL to a downscaled JPEG so the base64 payload
/// stays small enough for the Tauri IPC bridge and the cloud vision model.
function downscaleImage(dataUrl: string, maxEdge = 1568, quality = 0.85): Promise<string> {
  return new Promise((resolve) => {
    const image = new Image();
    image.onerror = () => resolve(dataUrl);
    image.onload = () => {
      try {
        const scale = Math.min(1, maxEdge / Math.max(image.width, image.height));
        const canvas = document.createElement('canvas');
        canvas.width = Math.max(1, Math.round(image.width * scale));
        canvas.height = Math.max(1, Math.round(image.height * scale));
        const context = canvas.getContext('2d');
        if (!context) {
          resolve(dataUrl);
          return;
        }
        context.drawImage(image, 0, 0, canvas.width, canvas.height);
        resolve(canvas.toDataURL('image/jpeg', quality));
      } catch {
        resolve(dataUrl);
      }
    };
    image.src = dataUrl;
  });
}

function errorMessage(reason: unknown) {
  if (reason instanceof Error) return reason.message;
  return String(reason).replace(/^Error:\s*/, '');
}

function loadHistory(): ResearchSession[] {
  try {
    const value = localStorage.getItem(HISTORY_KEY);
    if (!value) return [];
    const parsed = JSON.parse(value) as ResearchSession[];
    return parsed.map((session) => ({
      ...session,
      messages: session.messages.map((message) => ({
        ...message,
        images: message.images ?? [],
        thinking: message.thinking ?? '',
      })),
    }));
  } catch {
    return [];
  }
}

function formatDate(value: number) {
  return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(value);
}

function domain(value: string) {
  try {
    return new URL(value).hostname.replace(/^www\./, '');
  } catch {
    return 'source';
  }
}

function openExternal(event: React.MouseEvent<HTMLAnchorElement>, href?: string) {
  if (!href || !/^https?:\/\//.test(href)) return;
  event.preventDefault();
  void openUrl(href);
}

export default App;
