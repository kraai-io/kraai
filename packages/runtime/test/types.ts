import { createRuntime, type RuntimeResult, type SessionSnapshot, type SettingsDocument } from '..';

const runtime = createRuntime({ storage_root: '/tmp/kraai-client' });
const snapshot: Promise<RuntimeResult<SessionSnapshot>> = runtime.getSessionSnapshot('session');
const settings: Promise<RuntimeResult<SettingsDocument>> = runtime.getSettings();
void snapshot;
void settings;

async function consume() {
  const subscription = runtime.subscribe();
  const event = await subscription.next();
  if (event.type === 'event') {
    const sequence: number = event.value.sequence;
    void sequence;
    if (typeof event.value.event === 'object' && 'StreamChunk' in event.value.event) {
      const chunk: string = event.value.event.StreamChunk.chunk;
      void chunk;
    }
  } else if (event.type === 'lagged') {
    const skipped: number = event.skipped;
    void skipped;
  }
  subscription.close();
  await runtime.shutdown();
}

void consume;
