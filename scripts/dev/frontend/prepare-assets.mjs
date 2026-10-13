import { cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..', '..', '..');

const publicDir = path.join(repoRoot, 'public');
const targetDir = path.join(publicDir, 'js', 'lib');
const target = path.join(targetDir, 'hls.min.js');

function resolveHlsAsset(startDir) {
    let currentDir = startDir;
    while (true) {
        const candidate = path.join(currentDir, 'node_modules', 'hls.js', 'dist', 'hls.min.js');
        if (existsSync(candidate)) {
            return candidate;
        }
        const parentDir = path.dirname(currentDir);
        if (parentDir === currentDir) break;
        currentDir = parentDir;
    }
    throw new Error('Unable to locate node_modules/hls.js/dist/hls.min.js from current repo path');
}

const source = resolveHlsAsset(repoRoot);

await mkdir(publicDir, { recursive: true });
await Promise.all([
    rm(path.join(publicDir, 'index.html'), { force: true }),
    rm(path.join(publicDir, 'login.html'), { force: true }),
    rm(path.join(publicDir, 'base-path.js'), { force: true }),
    rm(path.join(publicDir, 'login.js'), { force: true }),
    rm(path.join(publicDir, 'logo.png'), { force: true }),
    rm(path.join(publicDir, 'output.css'), { force: true }),
    rm(path.join(publicDir, 'js'), { recursive: true, force: true }),
]);
await cp(path.join(repoRoot, 'web', 'pages', 'index.html'), path.join(publicDir, 'index.html'));
await cp(path.join(repoRoot, 'web', 'pages', 'login.html'), path.join(publicDir, 'login.html'));
await cp(path.join(repoRoot, 'web', 'assets', 'base-path.js'), path.join(publicDir, 'base-path.js'));
await cp(path.join(repoRoot, 'web', 'assets', 'login.js'), path.join(publicDir, 'login.js'));
await cp(path.join(repoRoot, 'web', 'assets', 'logo.png'), path.join(publicDir, 'logo.png'));

await mkdir(targetDir, { recursive: true });
const hlsBundle = await readFile(source, 'utf8');
await writeFile(
    target,
    hlsBundle.replace(/\n?\/\/# sourceMappingURL=hls\.min\.js\.map\s*$/, '\n'),
);

console.log(`Synced ${path.relative(repoRoot, target)} from hls.js dependency`);
