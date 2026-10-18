// Serves index.html + page.ts for the MoQ browser specs. `localhost` is a
// secure context, which WebTransport and WebCodecs require.
import { defineConfig } from 'vite';

export default defineConfig({
    root: import.meta.dirname,
    clearScreen: false,
    server: { host: 'localhost' },
});
