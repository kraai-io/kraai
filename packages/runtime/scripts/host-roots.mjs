import { existsSync } from 'node:fs';
import { posix } from 'node:path';

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

export function darwinRuntimeRoots(linkedLibraries) {
  const roots = linkedLibraries.split('\n').slice(1).filter(line => line.trim()).map(line => {
    const library = line.trim().split(' (')[0];
    if (library.startsWith('/nix/store/')) return '/nix/store';
    if (!posix.isAbsolute(library)) {
      throw new Error(`Unsupported host library path: ${library}`);
    }
    return posix.dirname(library);
  });
  return [...new Set(roots)];
}
