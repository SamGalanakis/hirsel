# C20-F2 — theme metadata bootstrap drift

Coordinator verdict: skip as a material audit recommendation. Worker ../workers/C20-WEB-SHELL.md. Reopened index.html bootstrap and theme.ts runtime; exact theme-color query repeated: 7 matches. Different dark/light hex colors are a real cosmetic drift, and comments are stale. However both normal paths apply the correct dark/light CSS class; the remaining difference is browser chrome tint. Invalid persisted theme strings require external/stale storage, while ordinary writes use the closed union.

The proposed HTML data-attribute ownership contract plus runtime DOM parsing/fallbacks and new bootstrap tests is disproportionate to two mismatched color constants. A simple palette correction can be a polish nit if root wants it; no state-model refactor or new issue promoted here. Preserve this distinction from F12, whose actual producer requests a status accessory and loses it.
