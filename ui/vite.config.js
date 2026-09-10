import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Plain Svelte, not SvelteKit: one page, no router, no SSR. Brief section 3.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "chrome110", emptyOutDir: true },
});
