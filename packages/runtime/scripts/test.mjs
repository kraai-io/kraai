import { execFileSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const directory = fileURLToPath(new URL('../test/', import.meta.url));
const skipSandbox = process.argv.includes('--skip-sandbox');
const files = readdirSync(directory).filter(name => name.endsWith('.test.cjs')
  && !(skipSandbox && name === 'scripts.test.cjs'));
execFileSync(process.execPath, ['--test', ...files], { cwd: directory, stdio: 'inherit' });
