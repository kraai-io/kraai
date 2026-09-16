import { execFileSync } from 'node:child_process';
import { constants, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { runtimeArtifacts } from './artifacts.mjs';
import { hostRuntimeRoots, darwinRuntimeRoots } from './host-roots.mjs';

const directory = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(directory, '../..');
const output = join(directory, 'dist');
const release = process.argv.includes('--release');
const profile = release ? ['--release'] : [];
const cargo = (args, options = {}) => execFileSync('cargo', args, { cwd: root, stdio: 'inherit', maxBuffer: 32 * 1024 * 1024, ...options });

const artifacts = runtimeArtifacts(cargo([
  'build', '--locked', '--message-format=json-render-diagnostics',
  '-p', 'kraai-runtime-node', '-p', 'kraai-nushell-runtime', ...profile,
], { stdio: ['ignore', 'pipe', 'inherit'], encoding: 'utf8' }));
const hostExecutable = basename(artifacts.host);
let runtimeRoots = [];
if (process.platform === 'linux') {
  const headers = execFileSync('readelf', ['--program-headers', artifacts.host], {
    encoding: 'utf8', env: { ...process.env, LC_ALL: 'C' },
  });
  runtimeRoots = hostRuntimeRoots(headers);
} else if (process.platform === 'darwin') {
  const libraries = execFileSync('otool', ['-L', artifacts.host], { encoding: 'utf8' });
  runtimeRoots = darwinRuntimeRoots(libraries);
}
const host = {
  executable: hostExecutable,
  runtime_roots: runtimeRoots,
};
mkdirSync(output, { recursive: true });
copyFileSync(artifacts.addon, join(output, 'kraai-runtime.node'), constants.COPYFILE_FICLONE);
copyFileSync(artifacts.host, join(output, hostExecutable), constants.COPYFILE_FICLONE);
writeFileSync(join(output, 'host.json'), JSON.stringify(host) + '\n');
const generated = mkdtempSync(join(tmpdir(), 'kraai-types-'));
try {
  execFileSync(artifacts.exporter, [generated], { cwd: root, stdio: 'inherit' });
  const types = readFileSync(join(generated, 'index.d.ts'), 'utf8');
  writeFileSync(join(output, 'index.d.ts'), types + '\nexport declare function createRuntime(options?: Partial<RuntimeOptions>): Runtime;\n');
} finally {
  rmSync(generated, { recursive: true, force: true });
}
