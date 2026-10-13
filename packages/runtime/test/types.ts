import { createRuntime, type MessageContent, type ModelSelection, type RuntimeResult, type SessionSnapshot, type SettingsDocument } from '..';

const runtime = createRuntime({ storage_root: '/tmp/kraai-client' });
const snapshot: Promise<RuntimeResult<SessionSnapshot>> = runtime.getSessionSnapshot('session');
const settings: Promise<RuntimeResult<SettingsDocument>> = runtime.getSettings();
const selection: ModelSelection = {
  provider_id: 'provider',
  model_id: 'model',
  options: { reasoning_effort: 'custom', fast: false, budget: 0 },
};
const selectedModel: Promise<RuntimeResult<ModelSelection | null>> = runtime.getSessionModel('session');
const savedModel: Promise<RuntimeResult<null>> = runtime.setSessionModel('session', selection);
void snapshot;
void settings;
void selectedModel;
void savedModel;

async function consume() {
  const subscription = runtime.subscribe();
  const event = await subscription.next();
  if (event.type === 'event') {
    const sequence: number = event.value.sequence;
    void sequence;
    if (typeof event.value.event === 'object' && 'ItemDelta' in event.value.event) {
      const delta: string = event.value.event.ItemDelta.delta;
      const index: number = event.value.event.ItemDelta.item_index;
      void delta;
      void index;
    }
    if (typeof event.value.event === 'object' && 'ItemStarted' in event.value.event) {
      const item = event.value.event.ItemStarted.item;
      if (item.type === 'script_call') {
        const input: string = item.input;
        const callId: string = item.call_id;
        void input;
        void callId;
      }
    }
  } else if (event.type === 'lagged') {
    const skipped: number = event.skipped;
    void skipped;
  }
  subscription.close();
  await runtime.shutdown();
}

void consume;

async function sendImage(bytes: number[]) {
  const imported = await runtime.importImage(bytes);
  if ('Err' in imported) return;
  const content: MessageContent = [
    { type: 'text', text: 'Inspect this image' },
    { type: 'image', image: imported.Ok },
  ];
  await runtime.sendContent('session', content, 'model', 'provider', {});
  const restored: RuntimeResult<MessageContent | null> = await runtime.undoLastUserMessage('session');
  void restored;
}

void sendImage;
