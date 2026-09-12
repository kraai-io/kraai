import { execFileSync } from 'node:child_process';
import { constants, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { hostRuntimeRoots } from './host-roots.mjs';

const directory = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(directory, '../..');
const output = join(directory, 'dist');
const release = process.argv.includes('--release');
const profile = release ? ['--release'] : [];
const cargo = (args, options = {}) => execFileSync('cargo', args, { cwd: root, stdio: 'inherit', ...options });

if (process.platform !== 'linux') {
  throw new Error('The Kraai sandbox currently requires Linux');
}

const metadata = JSON.parse(cargo(['metadata', '--format-version', '1', '--no-deps'], { stdio: 'pipe', encoding: 'utf8' }));
cargo(['build', '--locked', '-p', 'kraai-runtime-node', '-p', 'kraai-nushell-runtime', ...profile]);
const target = join(metadata.target_directory, release ? 'release' : 'debug');
const hostExecutable = 'kraai-nushell-host';
const programHeaders = execFileSync('readelf', ['--program-headers', join(target, hostExecutable)], {
  encoding: 'utf8',
  env: { ...process.env, LC_ALL: 'C' },
});
const host = {
  executable: hostExecutable,
  runtime_roots: hostRuntimeRoots(programHeaders),
};
mkdirSync(output, { recursive: true });
copyFileSync(join(target, 'libkraai_runtime_node.so'), join(output, 'kraai-runtime.node'), constants.COPYFILE_FICLONE);
copyFileSync(join(target, hostExecutable), join(output, hostExecutable), constants.COPYFILE_FICLONE);
writeFileSync(join(output, 'host.json'), JSON.stringify(host) + '\n');
const generated = mkdtempSync(join(tmpdir(), 'kraai-types-'));
try {
  execFileSync(join(target, 'export-types'), [generated], { cwd: root, stdio: 'inherit' });
  const types = readFileSync(join(generated, 'index.d.ts'), 'utf8');
  writeFileSync(join(output, 'index.d.ts'), types + '\nexport declare function createRuntime(options?: Partial<RuntimeOptions>): Runtime;\n');
} finally {
  rmSync(generated, { recursive: true, force: true });
}
