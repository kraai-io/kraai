import { existsSync } from 'node:fs';

export function hostRuntimeRoots(programHeaders, exists = existsSync) {
  const interpreter = programHeaders.match(/\[Requesting program interpreter: ([^\]]+)\]/)?.[1];
  if (!interpreter) return [];
  if (interpreter.startsWith('/nix/store/')) return ['/nix/store'];
  const libraryRoots = ['/lib', '/lib64', '/usr/lib', '/usr/lib64'];
  if (!libraryRoots.some(root => interpreter.startsWith(`${root}/`))) {
    throw new Error(`Unsupported host dynamic loader: ${interpreter}`);
  }
  return [...libraryRoots, '/etc/ld.so.cache'].filter(exists);
}
