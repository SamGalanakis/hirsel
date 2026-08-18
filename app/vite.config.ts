import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";
import { VitePWA } from "vite-plugin-pwa";

const devProxy = new URL(process.env.HIRSEL_DEV_PROXY_TARGET ?? "ws://127.0.0.1:8787");
const devWsTarget = `${devProxy.protocol}//${devProxy.host}`;
const devHttpTarget = `${devProxy.protocol === "wss:" ? "https:" : "http:"}//${devProxy.host}`;

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    solid(),
    tailwindcss(),
    VitePWA({
      registerType: "autoUpdate",
      // Slice 1 scope: precache the app shell only. No push, no background
      // sync (see docs/SCOPE.md) - keep the service worker boring.
      workbox: {
        globPatterns: ["**/*.{js,css,html,svg,woff2}"],
      },
      manifest: {
        name: "hirsel",
        short_name: "hirsel",
        description: "Single-player personal agent client",
        theme_color: "#141414",
        background_color: "#141414",
        display: "standalone",
        icons: [
          {
            src: "/icon.svg",
            sizes: "any",
            type: "image/svg+xml",
          },
        ],
      },
    }),
  ],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  // Same-origin dev proxy: the browser talks only to Vite and Vite forwards
  // WS + blob traffic to the local mock by default. Point
  // HIRSEL_DEV_PROXY_TARGET at a real Host when needed. This keeps remote
  // development and port forwarding to a single browser-visible origin.
  server: {
    proxy: {
      "/ws": { target: devWsTarget, ws: true, changeOrigin: true },
      "/blob": { target: devHttpTarget, changeOrigin: true },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: "./vitest.setup.ts",
    css: true,
    include: ["src/**/*.test.{ts,tsx}"],
    // The component tests dynamic-import a fresh module graph per test (for
    // store isolation); first-load transform of that graph can exceed the 5s
    // default, so give tests more headroom.
    testTimeout: 20000,
  },
});
