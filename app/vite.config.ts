/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    react(),
    VitePWA({
      registerType: "autoUpdate",
      // Slice 1 scope: precache the app shell only. No push, no background
      // sync (see docs/SCOPE.md) - keep the service worker boring.
      workbox: {
        globPatterns: ["**/*.{js,css,html,svg}"],
      },
      manifest: {
        name: "hirsel",
        short_name: "hirsel",
        description: "Single-player personal agent client",
        theme_color: "#0b0f14",
        background_color: "#0b0f14",
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
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
