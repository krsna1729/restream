import { cp, mkdir, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..', '..', '..');

const publicDir = path.join(repoRoot, 'public');
await mkdir(publicDir, { recursive: true });
await Promise.all([
    rm(path.join(publicDir, 'index.html'), { force: true }),
    rm(path.join(publicDir, 'login.html'), { force: true }),
    rm(path.join(publicDir, 'base-path.js'), { force: true }),
    rm(path.join(publicDir, 'login.js'), { force: true }),
    rm(path.join(publicDir, 'sw.js'), { force: true }),
    rm(path.join(publicDir, 'logo.png'), { force: true }),
    rm(path.join(publicDir, 'output.css'), { force: true }),
    rm(path.join(publicDir, 'js'), { recursive: true, force: true }),
]);
await cp(path.join(repoRoot, 'web', 'pages', 'index.html'), path.join(publicDir, 'index.html'));
await cp(path.join(repoRoot, 'web', 'pages', 'login.html'), path.join(publicDir, 'login.html'));
await cp(path.join(repoRoot, 'web', 'assets', 'base-path.js'), path.join(publicDir, 'base-path.js'));
await cp(path.join(repoRoot, 'web', 'assets', 'login.js'), path.join(publicDir, 'login.js'));
await cp(path.join(repoRoot, 'web', 'assets', 'sw.js'), path.join(publicDir, 'sw.js'));
await cp(path.join(repoRoot, 'web', 'assets', 'logo.png'), path.join(publicDir, 'logo.png'));

console.log('Synced pages and assets into public/');
