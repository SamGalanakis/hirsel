/// <reference types="vitest/config" />
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";
import { VitePWA } from "vite-plugin-pwa";

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
