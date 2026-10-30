import { cp, mkdir, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..', '..', '..');

// public/ name -> source under web/.
const assets = {
    'index.html': 'pages/index.html',
    'login.html': 'pages/login.html',
    'base-path.js': 'assets/base-path.js',
    'login.js': 'assets/login.js',
    'sw.js': 'assets/sw.js',
    'logo.png': 'assets/logo.png',
};

const publicDir = path.join(repoRoot, 'public');
await mkdir(publicDir, { recursive: true });
// output.css and js/ are rebuilt by Tailwind and Vite after this step.
await Promise.all(
    [...Object.keys(assets), 'output.css', 'js'].map((name) =>
        rm(path.join(publicDir, name), { recursive: true, force: true }),
    ),
);
await Promise.all(
    Object.entries(assets).map(([name, source]) =>
        cp(path.join(repoRoot, 'web', source), path.join(publicDir, name)),
    ),
);

console.log('Synced pages and assets into public/');
