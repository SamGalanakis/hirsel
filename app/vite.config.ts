import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";
import { VitePWA } from "vite-plugin-pwa";

// Plugin UI modules are compiled from <repo-root>/plugins/<id>/ui, OUTSIDE this
// app's root and outside the tree that reaches app/node_modules by walking up,
// so their bare imports (`solid-js`, and anything else the app depends on) have
// nowhere to resolve from. This resolves them AS IF the import came from the
// app's own source: same node_modules, same package-exports conditions, same
// single Solid instance — which is what makes a plugin's components reactive
// with the app's. A blanket alias would not do: it bypasses package exports and
// silently hands out Solid's server build.
const pluginUiRoot = fileURLToPath(new URL("../plugins", import.meta.url));
const appSourceAnchor = fileURLToPath(new URL("./src/main.tsx", import.meta.url));

function resolvePluginUiImports() {
  return {
    name: "hirsel:plugin-ui-imports",
    enforce: "pre" as const,
    async resolveId(this: unknown, source: string, importer: string | undefined) {
      if (!importer || !importer.startsWith(pluginUiRoot)) return null;
      // Bare specifiers only — relative and absolute paths resolve normally.
      if (/^[./]/.test(source) || source.startsWith("\0")) return null;
      const resolver = this as {
        resolve: (
          s: string,
          i: string,
          o: { skipSelf: boolean },
        ) => Promise<{ id: string } | null>;
      };
      const resolved = await resolver.resolve(source, appSourceAnchor, { skipSelf: true });
      return resolved?.id ?? null;
    },
  };
}

const devProxy = new URL(process.env.HIRSEL_DEV_PROXY_TARGET ?? "ws://127.0.0.1:8787");
const devWsTarget = `${devProxy.protocol}//${devProxy.host}`;
const devHttpTarget = `${devProxy.protocol === "wss:" ? "https:" : "http:"}//${devProxy.host}`;

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    resolvePluginUiImports(),
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
    // Plugin UI modules live at <repo-root>/plugins/<id>/ui, outside this app's
    // root, and are pulled in by the glob in src/plugins/loader.ts. The dev
    // server must be allowed to serve them; the production build inlines them
    // into per-plugin chunks and needs nothing here.
    fs: {
      allow: [
        fileURLToPath(new URL(".", import.meta.url)),
        fileURLToPath(new URL("../plugins", import.meta.url)),
      ],
    },
    proxy: {
      "/ws": { target: devWsTarget, ws: true, changeOrigin: true },
      "/blob": { target: devHttpTarget, changeOrigin: true },
      // Plugin tier: the roster/administration REST surface plus the per-plugin
      // routers a plugin's own UI calls. Same one-origin rule as /ws and /blob —
      // the browser only ever talks to Vite.
      "/api": { target: devHttpTarget, changeOrigin: true },
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
