"""Shared HTML shell, editorial CSS, SVG icons, and client script for Hirsel's docs pages.

Visual tokens match site/index.html (warm parchment cream, oat surfaces, deep charcoal ink,
one signature crimson, earth-toned Tree-sitter syntax highlighting, Archivo Black + Inter)
arranged in a crisp left-sidebar developer-docs shell.
"""

from __future__ import annotations

import html
import json

ORIGIN = "https://hirsel.ai"
GITHUB_REPO = "https://github.com/SamGalanakis/hirsel"

AGENT_COMMENT = (
    "<!-- For coding agents: https://hirsel.ai/llms.txt | /llms-full.txt | "
    "/primer.md | /cookbook.md | /api-index.md — or run `hirsel primer` "
    "from your installed binary -->"
)

RAM_MARK_SVG = (
    '<svg class="mark" viewBox="0 0 512 512" width="24" height="24" aria-hidden="true">'
    '<rect width="512" height="512" rx="96" fill="#CB271F"/>'
    '<path d="M96.91,16.08 Q95.84,15.44 94.76,16.07 L27.58,55.01 Q26.50,55.64 26.50,56.89 '
    'L26.50,122.73 Q26.50,123.98 27.54,124.67 L63.64,148.42 Q64.69,149.11 64.69,150.36 '
    'L64.69,204.13 Q64.69,205.38 65.76,206.02 L80.69,214.80 Q81.77,215.44 83.00,215.23 '
    'L86.57,214.64 Q87.80,214.43 87.84,213.18 L88.77,183.52 Q88.81,182.27 90.04,182.45 '
    'L94.60,183.10 Q95.84,183.27 96.88,183.97 L112.89,194.64 Q113.93,195.33 113.93,196.58 '
    'L113.93,231.27 Q113.93,232.52 114.97,233.21 L125.03,239.92 Q125.99,240.56 127.15,240.56 '
    'L129.86,240.56 Q131.01,240.56 131.96,239.90 L140.04,234.24 Q141.06,233.52 141.06,232.27 '
    'L141.06,200.61 Q141.06,199.36 142.15,198.74 L200.64,165.60 Q201.36,165.19 202.06,165.65 '
    'L203.69,166.73 Q204.38,167.19 204.38,168.03 L204.38,202.13 Q204.38,203.38 205.54,203.84 '
    'L208.25,204.92 Q209.40,205.38 210.51,204.81 L227.39,195.92 Q228.50,195.33 228.52,194.09 '
    'L229.48,135.28 Q229.50,134.03 228.44,133.38 L186.27,107.50 Q185.28,106.89 184.13,106.89 '
    'L181.42,106.89 Q180.26,106.89 179.26,107.47 L116.02,143.46 Q114.94,144.08 113.88,143.41 '
    'L63.38,111.36 Q62.67,110.92 62.21,110.22 L61.13,108.59 Q60.66,107.90 60.69,107.07 '
    'L61.64,71.96 Q61.67,70.72 62.73,70.06 L94.78,50.27 Q95.84,49.61 96.93,50.22 '
    'L119.87,63.07 Q120.96,63.68 120.96,64.93 L120.96,80.52 Q120.96,81.77 119.92,82.46 '
    'L106.71,91.28 Q105.89,91.82 105.20,91.13 L103.57,89.50 Q102.88,88.81 102.88,87.82 '
    'L102.88,76.90 Q102.88,75.74 101.95,75.05 L99.78,73.42 Q98.85,72.73 97.72,72.95 '
    'L94.99,73.50 Q93.83,73.73 92.84,74.36 L83.83,80.09 Q82.78,80.77 82.83,82.01 '
    'L83.72,101.63 Q83.78,102.88 84.84,103.54 L101.90,114.32 Q102.88,114.94 104.03,114.94 '
    'L106.74,114.94 Q107.90,114.94 108.89,114.34 L145.02,92.47 Q146.09,91.82 146.09,90.57 '
    'L146.09,46.84 Q146.09,45.59 145.02,44.95 L96.91,16.08 Z" '
    'transform="translate(50 44) scale(1.609375)" fill="#FBF6EC"/></svg>'
)

COPY_ICON = (
    '<svg class="ic-copy" viewBox="0 0 16 16" aria-hidden="true" fill="none" '
    'stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">'
    '<rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/>'
    '<path d="M10.5 5.5V3.5a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2"/></svg>'
)

OK_ICON = (
    '<svg class="ic-ok" viewBox="0 0 16 16" aria-hidden="true" fill="none" '
    'stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">'
    '<path d="M3.2 8.4 6.5 11.7 12.8 4.8"/></svg>'
)

GITHUB_ICON = (
    '<svg viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">'
    '<path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 '
    '0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53'
    '.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 '
    '0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 '
    '1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 '
    '3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 '
    '8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>'
)

MENU_ICON = (
    '<svg viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="currentColor" '
    'stroke-width="1.6" stroke-linecap="round">'
    '<path d="M3.5 6h13M3.5 10h13M3.5 14h9"/></svg>'
)

CLOSE_ICON = (
    '<svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" '
    'stroke-width="1.6" stroke-linecap="round">'
    '<path d="M3.5 3.5l9 9M12.5 3.5l-9 9"/></svg>'
)

TOPO_ACCENT_SVG = (
    '<svg class="topo" viewBox="0 0 320 110" aria-hidden="true" fill="none">'
    '<path d="M12 104C68 82 114 34 192 26c56-6 92 14 126 30" stroke="currentColor" stroke-width="1"/>'
    '<path d="M38 106C94 84 134 48 206 42c48-4 82 12 112 28" stroke="currentColor" stroke-width="1"/>'
    '<path d="M72 108C124 88 158 62 222 58c40-2 68 10 96 26" stroke="currentColor" stroke-width="1"/>'
    '<path d="M114 108C158 92 186 76 238 74c30-1 54 9 80 22" stroke="currentColor" stroke-width="1"/>'
    '</svg>'
)

DOCS_CSS = """
@font-face{font-family:'Archivo Black';font-style:normal;font-weight:400;font-display:swap;src:url(/fonts/archivo-black-latin.woff2) format('woff2')}
@font-face{font-family:'Inter';font-style:normal;font-weight:400 600;font-display:swap;src:url(/fonts/inter-latin.woff2) format('woff2')}
:root{
  --ink:#11110F;--body:#2B2A26;--cream:#FBF6EC;--paper:#F4EDE0;--ground:#ECE3CF;--code-bg:#F2EBD9;
  --red:#CB271F;--red-soft:rgba(203,39,31,.08);--mute:#5E5A50;
  --rule:rgba(17,17,15,.14);--rule-strong:rgba(17,17,15,.26);
  --umber:#80552A;--moss:#4F6A2E;--faint:rgba(17,17,15,.48);
  --ease:cubic-bezier(.16,1,.3,1);--t-press:110ms;--t-state:220ms;
  --top:58px;--side:272px;--measure:68ch;
  --mono:ui-monospace,"SF Mono",SFMono-Regular,Menlo,Consolas,"DejaVu Sans Mono","Liberation Mono",monospace;
  --display:"Archivo Black","Arial Black",Inter,system-ui,sans-serif;
  --sans:Inter,system-ui,-apple-system,"Segoe UI",sans-serif;
}
::selection{background:rgba(203,39,31,.16);color:var(--ink)}
*{box-sizing:border-box}
[hidden]{display:none!important}
html{scroll-behavior:smooth;background:var(--cream);color:var(--ink);-webkit-text-size-adjust:100%}
body{margin:0;min-height:100vh;background:var(--cream);color:var(--body);font:400 16px/1.66 var(--sans);font-kerning:normal;-webkit-font-smoothing:antialiased}
a{color:inherit}
button{font:inherit;color:inherit;background:none;border:0;padding:0;cursor:pointer}
:focus-visible{outline:2px solid var(--ink);outline-offset:2px}
.wmfont{font-family:var(--display);font-weight:400;font-synthesis:none;letter-spacing:-.035em}

/* Sticky top bar */
header.top{
  position:sticky;top:0;z-index:30;height:var(--top);
  display:flex;align-items:center;gap:20px;
  padding:0 24px;
  background:rgba(251,246,236,.92);
  -webkit-backdrop-filter:blur(10px);backdrop-filter:blur(10px);
  border-bottom:1px solid var(--rule);
}
header.top .menu-btn{
  display:none;align-items:center;justify-content:center;
  width:38px;height:38px;margin-left:-8px;border-radius:8px;color:var(--ink);flex:none;
}
header.top .menu-btn:hover{background:rgba(17,17,15,.06)}
header.top .menu-btn svg{width:20px;height:20px;display:block}
header.top .brand{
  display:inline-flex;align-items:center;gap:9px;text-decoration:none;color:var(--ink);flex:none;
}
header.top .brand .mark{width:24px;height:24px;display:block;flex:none}
header.top .brand .wm{font-size:20px;line-height:1}
header.top .nav{display:flex;align-items:center;gap:20px;min-width:0;overflow-x:auto;scrollbar-width:none}
header.top .nav::-webkit-scrollbar{display:none}
header.top .nav a{
  position:relative;font-size:14px;font-weight:500;color:var(--mute);text-decoration:none;
  padding:5px 0 6px;white-space:nowrap;transition:color var(--t-state) var(--ease);flex:none;
}
header.top .nav a::after{
  content:"";position:absolute;left:0;right:0;bottom:0;height:1.5px;background:currentColor;
  transform:scaleX(0);transform-origin:left;opacity:.4;transition:transform var(--t-state) var(--ease),opacity var(--t-state) var(--ease);
}
header.top .nav a:hover,header.top .nav a:focus-visible{color:var(--ink)}
header.top .nav a:hover::after,header.top .nav a:focus-visible::after{transform:scaleX(1)}
header.top .nav a[aria-current="page"]{color:var(--ink)}
header.top .nav a[aria-current="page"]::after{transform:scaleX(1);opacity:1;background:var(--red)}
header.top .right{margin-left:auto;display:flex;align-items:center;gap:10px;flex:none}
header.top .hill-link{
  display:inline-flex;align-items:center;gap:6px;font-size:13px;font-weight:500;
  color:var(--mute);text-decoration:none;padding:6px 10px;border-radius:999px;
  transition:color var(--t-state) var(--ease),background-color var(--t-state) var(--ease);
  white-space:nowrap;
}
header.top .hill-link .dot{width:8px;height:8px;background:var(--red);clip-path:polygon(50% 0,100% 50%,50% 100%,0 50%);flex:none}
header.top .hill-link:hover,header.top .hill-link:focus-visible{color:var(--ink);background:rgba(17,17,15,.045)}
.pill-copy{
  position:relative;display:inline-flex;align-items:center;gap:6px;
  font-size:13px;font-weight:500;line-height:1;color:var(--mute);
  padding:6px 11px;border:1px solid var(--rule);border-radius:999px;white-space:nowrap;
  transition:color var(--t-state) var(--ease),border-color var(--t-state) var(--ease),background-color var(--t-state) var(--ease),transform var(--t-press) var(--ease);
}
.pill-copy:hover,.pill-copy:focus-visible{color:var(--ink);border-color:var(--rule-strong);background:rgba(17,17,15,.035)}
.pill-copy:active{transform:scale(.97)}
.pill-copy.ok{color:var(--ink);border-color:var(--ink)}
.pill-copy svg{width:13px;height:13px;flex:none}
.pill-copy .ic-ok,.pill-copy.ok .ic-copy{display:none}
.pill-copy.ok .ic-ok{display:block}
.pill-copy .lbl{display:grid;text-align:left}
.pill-copy .lbl span{grid-area:1/1;transition:opacity var(--t-press) var(--ease)}
.pill-copy .lbl .done,.pill-copy.ok .lbl .idle{opacity:0}
.pill-copy.ok .lbl .done{opacity:1}
header.top .gh{
  display:inline-flex;align-items:center;justify-content:center;
  width:34px;height:34px;margin-right:-6px;border-radius:50%;color:var(--ink);opacity:.82;
  transition:opacity var(--t-state) var(--ease),background-color var(--t-state) var(--ease);
}
header.top .gh:hover,header.top .gh:focus-visible{opacity:1;background:rgba(17,17,15,.06)}
header.top .gh svg{width:19px;height:19px;display:block}

/* Two-column docs shell */
.shell{display:flex;align-items:flex-start;max-width:1360px;margin:0 auto}
aside.sidebar{
  flex:0 0 var(--side);width:var(--side);
  position:sticky;top:var(--top);
  height:calc(100vh - var(--top));overflow-y:auto;
  padding:22px 16px 40px 22px;
  border-right:1px solid var(--rule);
  scrollbar-width:thin;scrollbar-color:var(--rule) transparent;
}
aside.sidebar .drawer-head{display:none;align-items:center;justify-content:space-between;padding:0 0 12px;margin-bottom:14px;border-bottom:1px solid var(--rule)}
aside.sidebar .drawer-head .wm{font-size:18px;color:var(--ink)}
aside.sidebar .drawer-close{width:36px;height:36px;display:inline-flex;align-items:center;justify-content:center;border-radius:8px;color:var(--mute)}
aside.sidebar .drawer-close:hover{background:rgba(17,17,15,.06);color:var(--ink)}
aside.sidebar .drawer-close svg{width:16px;height:16px}
.side-group{margin-bottom:22px}
.side-label{
  margin:0 0 8px 8px;font:600 11px/1.2 var(--mono);
  letter-spacing:.08em;text-transform:uppercase;color:var(--mute);
}
.side-list{list-style:none;margin:0;padding:0}
.side-list>li{margin:2px 0}
.side-page{
  display:flex;align-items:center;justify-content:space-between;gap:8px;
  padding:6px 10px;border-radius:7px;
  font-size:14px;font-weight:500;color:var(--mute);text-decoration:none;
  transition:color var(--t-state) var(--ease),background-color var(--t-state) var(--ease);
}
.side-page:hover,.side-page:focus-visible{color:var(--ink);background:rgba(17,17,15,.04)}
.side-page[aria-current="page"]{color:var(--ink);font-weight:600;background:var(--paper)}
.side-page .tag{font:500 11px/1 var(--mono);color:var(--mute)}
.toc-sub{
  list-style:none;margin:6px 0 10px 10px;padding:0 0 0 10px;
  border-left:1px solid var(--rule);
}
.toc-sub li{margin:1px 0}
.toc-sub a{
  display:block;padding:4px 8px;margin-left:-11px;padding-left:12px;
  border-left:2px solid transparent;
  font-size:13px;line-height:1.38;color:var(--mute);text-decoration:none;
  transition:color var(--t-state) var(--ease),border-color var(--t-state) var(--ease);
}
.toc-sub a:hover,.toc-sub a:focus-visible{color:var(--ink)}
.toc-sub a.active{color:var(--ink);font-weight:600;border-left-color:var(--red)}
a.side-label{display:block;text-decoration:none;padding:4px 8px;margin:0 0 4px;border-radius:6px;transition:color var(--t-state) var(--ease)}
a.side-label:hover,a.side-label:focus-visible{color:var(--ink)}
a.side-label.active{color:var(--red)}
.toc-sub a code{font-family:var(--mono);font-size:.92em;background:none;border:0;padding:0}
.side-foot{margin-top:24px;padding-top:16px;border-top:1px solid var(--rule)}
.side-foot a{
  display:flex;align-items:center;justify-content:space-between;gap:8px;
  padding:8px 10px;border-radius:8px;text-decoration:none;
  font-size:13.5px;font-weight:500;color:var(--mute);
  transition:color var(--t-state) var(--ease),background-color var(--t-state) var(--ease);
}
.side-foot a:hover,.side-foot a:focus-visible{color:var(--ink);background:var(--paper)}
.side-foot .sub{font-size:11.5px;color:var(--mute)}
.backdrop{display:none;position:fixed;inset:0;z-index:35;background:rgba(17,17,15,.26)}

/* Main editorial reading column */
main.doc{
  flex:1 1 auto;min-width:0;max-width:820px;
  margin:0 auto;padding:38px 44px 92px;
}
.hero-doc{
  position:relative;overflow:hidden;
  padding:0 0 24px;margin-bottom:28px;border-bottom:1px solid var(--rule);
}
.hero-doc .topo{
  position:absolute;right:-16px;top:-12px;width:280px;height:auto;
  color:rgba(17,17,15,.08);pointer-events:none;
}
.eyebrow{
  display:inline-flex;align-items:center;gap:8px;margin:0 0 12px;
  font:500 12px/1 var(--mono);letter-spacing:.07em;text-transform:uppercase;color:var(--mute);
}
.eyebrow .pip{width:7px;height:7px;border-radius:50%;background:var(--red);flex:none}
.hero-doc h1{
  margin:0 0 12px;font-family:var(--display);font-weight:400;font-synthesis:none;
  font-size:clamp(30px,4.4vw,44px);line-height:1.06;letter-spacing:-.03em;color:var(--ink);
  max-width:20ch;
}
.hero-doc .lead{margin:0;font-size:17px;line-height:1.58;color:var(--body);max-width:60ch;text-wrap:pretty}
.release-note{
  margin:18px 0 24px;padding:12px 16px;border-radius:8px;
  background:var(--paper);border:1px solid var(--rule);border-left:3px solid var(--red);
  font-size:14px;line-height:1.55;color:var(--body);
}
.release-note p{margin:0}

/* Parts of the one docs page: Install, Language, Cookbook */
.part{margin-top:88px;padding-top:30px;border-top:2px solid var(--ink)}
.part.first{margin-top:0;padding-top:0;border-top:0}
.part-no{margin:0 0 6px;font:500 12px/1 var(--mono);letter-spacing:.08em;color:var(--red)}
main.doc h2.part-title{
  margin:0 0 12px;padding:0;border:0;
  font-size:clamp(28px,3.6vw,36px);line-height:1.08;letter-spacing:-.03em;
}
main.doc .part-lead{font-size:16.5px;line-height:1.6;color:var(--body);margin-bottom:22px}

/* Prose */

:where(main.doc) h2{
  position:relative;margin:48px 0 16px;padding-bottom:10px;
  border-bottom:1px solid var(--rule);
  font-family:var(--display);font-weight:400;font-synthesis:none;
  font-size:23px;line-height:1.2;letter-spacing:-.022em;color:var(--ink);
  scroll-margin-top:78px;
}
:where(main.doc) h2 code{font-family:var(--mono);font-size:.82em;font-weight:500;letter-spacing:0}
:where(main.doc) h3{
  margin:30px 0 10px;font:600 18px/1.3 var(--sans);letter-spacing:-.015em;color:var(--ink);
  scroll-margin-top:78px;
}
:where(main.doc) .anchor{
  margin-left:8px;color:var(--mute);text-decoration:none;font-family:var(--sans);
  font-weight:400;font-size:.78em;opacity:0;transition:opacity var(--t-state) var(--ease),color var(--t-state) var(--ease);
}
:where(main.doc) :is(h2,h3):is(:hover,:focus-within) .anchor{opacity:.85}
:where(main.doc) .anchor:hover{color:var(--red)}
:where(main.doc) p{margin:0 0 15px;max-width:var(--measure);text-wrap:pretty}
:where(main.doc) :is(ul,ol){margin:0 0 18px;padding-left:22px;max-width:var(--measure)}
:where(main.doc) li{margin:0 0 8px}
:where(main.doc) li>p{margin:0 0 8px}
main.doc li .code-block{margin:10px 0 14px;max-width:none}
:where(main.doc) strong{font-weight:600;color:var(--ink)}
:where(main.doc) a:where(:not(.anchor,.nav-card,.pill-btn,.mod-link)){
  color:var(--ink);text-decoration:underline;text-decoration-color:rgba(203,39,31,.42);
  text-underline-offset:3px;text-decoration-thickness:1.5px;
  transition:color var(--t-state) var(--ease),text-decoration-color var(--t-state) var(--ease);
}
:where(main.doc) a:where(:not(.anchor,.nav-card,.pill-btn,.mod-link)):hover{color:var(--red);text-decoration-color:var(--red)}
:where(main.doc) hr{border:0;border-top:1px solid var(--rule);margin:36px 0}
:where(main.doc) blockquote{
  margin:18px 0;padding:12px 18px;max-width:var(--measure);
  background:var(--paper);border-left:3px solid var(--red);border-radius:0 8px 8px 0;
  color:var(--body);font-size:15px;
}
:where(main.doc) blockquote p:last-child{margin-bottom:0}
code.w{white-space:normal}
:where(main.doc) :not(pre)>code{
  font-family:var(--mono);font-size:.86em;white-space:nowrap;-webkit-box-decoration-break:clone;box-decoration-break:clone;
  background:rgba(17,17,15,.065);border:1px solid rgba(17,17,15,.09);
  padding:.1em .38em;border-radius:5px;color:var(--ink);
}

/* Code blocks with Tree-sitter highlighting */
.code-block{
  margin:16px 0 20px;border-radius:10px;overflow:hidden;
  background:var(--code-bg);border:1px solid rgba(17,17,15,.13);
}
.code-block.is-output{background:var(--paper);border-style:dashed;border-color:rgba(17,17,15,.22)}
.code-bar{
  display:flex;align-items:center;justify-content:space-between;gap:12px;
  min-height:34px;padding:4px 12px 4px 14px;
  background:rgba(17,17,15,.038);border-bottom:1px solid rgba(17,17,15,.08);
}
.code-meta{display:inline-flex;align-items:center;gap:8px;min-width:0}
.code-lang{font:500 11.5px/1 var(--mono);letter-spacing:.03em;color:var(--mute)}
.code-badge{
  font:500 11px/1 var(--mono);padding:3px 7px;border-radius:999px;
  background:var(--red-soft);color:var(--red);white-space:nowrap;
}
.code-copy{
  display:inline-flex;align-items:center;gap:5px;
  font:500 11.5px/1 var(--sans);color:var(--mute);
  padding:4px 8px;border-radius:6px;
  transition:color var(--t-state) var(--ease),background-color var(--t-state) var(--ease);
}
.code-copy:hover,.code-copy:focus-visible{color:var(--ink);background:rgba(17,17,15,.06)}
.code-copy.ok{color:var(--ink)}
.code-copy svg{width:12.5px;height:12.5px;flex:none}
.code-copy .ic-ok,.code-copy.ok .ic-copy{display:none}
.code-copy.ok .ic-ok{display:block}
.code-block pre{
  margin:0;padding:14px 0;overflow-x:auto;
  font:400 13.5px/1.62 var(--mono);color:var(--ink);
  tab-size:2;font-variant-ligatures:none;
}
/* Lines are inline spans joined by real newlines, so copy and innerText round-trip exactly.
   code is at least as wide as pre and as wide as its longest line, so a refused-line band
   spans the whole scroll width. */
.code-block pre code{display:inline-block;box-sizing:border-box;min-width:100%;background:none;border:0;padding:0 16px;font-size:inherit}
.code-block .ln.no{
  display:inline-block;box-sizing:border-box;width:calc(100% + 32px);margin:0 -16px;padding:0 16px 0 13px;
  background:rgba(203,39,31,.075);border-left:3px solid var(--red);
}
.code-block .k{font-weight:600;color:var(--ink)}
.code-block .d{font-weight:600;color:var(--umber)}
.code-block .t{color:var(--umber)}
.code-block .v{color:var(--moss)}
.code-block .o{color:var(--faint)}
.code-block .c{color:var(--mute)}
.code-block .ln.no .c{color:var(--red);font-weight:500}
.outcome-note{
  margin:-10px 0 20px;font:500 13px/1.4 var(--mono);color:var(--moss);
}

/* Tables */
.table-wrap{
  margin:18px 0 24px;overflow-x:auto;
  border:1px solid var(--rule);border-radius:10px;background:var(--cream);
}
table{width:100%;border-collapse:collapse;font-size:14.5px;line-height:1.5}
thead th{
  background:var(--ground);color:var(--ink);font-weight:600;font-size:13px;
  text-align:left;padding:10px 14px;border-bottom:1px solid var(--rule);white-space:nowrap;
}
tbody td{padding:10px 14px;border-top:1px solid var(--rule);vertical-align:top;color:var(--body)}
tbody tr:first-child td{border-top:0}
tbody tr:hover{background:rgba(236,227,207,.35)}
td code{white-space:nowrap}

/* Std library filter & badges */
.std-controls{
  display:flex;align-items:center;gap:12px;flex-wrap:wrap;margin:20px 0 16px;
}
.std-search{
  flex:1 1 240px;min-width:0;padding:9px 14px;border-radius:8px;
  border:1px solid var(--rule-strong);background:var(--cream);color:var(--ink);
  font:400 14.5px/1.3 var(--sans);
}
.std-search::placeholder{color:var(--mute)}
.std-search:focus{outline:2px solid var(--ink);outline-offset:-1px;border-color:var(--ink)}
.std-filters{display:inline-flex;gap:6px}
.pill-btn{
  padding:7px 12px;border-radius:999px;border:1px solid var(--rule);
  font-size:13px;font-weight:500;color:var(--mute);text-decoration:none;
  transition:color var(--t-state) var(--ease),background-color var(--t-state) var(--ease),border-color var(--t-state) var(--ease);
}
.pill-btn:hover{color:var(--ink);border-color:var(--rule-strong)}
.pill-btn[aria-pressed="true"]{color:var(--ink);background:var(--ground);border-color:var(--ink)}
.std-count{font:500 12.5px/1 var(--mono);color:var(--mute);margin:0 0 12px}
.mod-link{font:600 13.5px/1.4 var(--mono);color:var(--ink);text-decoration:none;border-bottom:1px solid rgba(203,39,31,.4)}
.mod-link:hover{color:var(--red);border-bottom-color:var(--red)}
.ext-badge{
  display:inline-block;padding:2px 7px;border-radius:999px;
  background:var(--ground);color:var(--umber);font:600 11px/1.3 var(--mono);white-space:nowrap;
}
.ops-cell{font:400 12.5px/1.6 var(--mono);color:var(--body);word-break:break-word}
.ops-num{font:500 13px/1.4 var(--mono);color:var(--mute);text-align:right;white-space:nowrap}

/* Install page cards & source drawer */
.info-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin:18px 0 28px}
.info-card{padding:16px 18px;border-radius:10px;background:var(--paper);border:1px solid var(--rule)}
.info-card h3{margin:0 0 6px;font:600 15px/1.3 var(--sans);color:var(--ink)}
.info-card p{margin:0;font-size:14px;line-height:1.52;color:var(--mute)}
.source-drawer{
  margin:22px 0 0;border:1px solid var(--rule);border-radius:10px;background:var(--paper);overflow:hidden;
}
.source-drawer summary{
  list-style:none;cursor:pointer;padding:13px 16px;
  display:flex;align-items:center;justify-content:space-between;gap:12px;
  font:500 14px/1.3 var(--sans);color:var(--ink);
}
.source-drawer summary::-webkit-details-marker{display:none}
.source-drawer summary .hint{font:500 12px/1 var(--mono);color:var(--mute)}
.source-drawer[open] summary{border-bottom:1px solid var(--rule)}
.source-drawer .code-block{margin:0;border:0;border-radius:0}

/* Prev / next footer */
.doc-pager{
  display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;
  margin-top:56px;padding-top:24px;border-top:1px solid var(--rule);
}
.nav-card{
  display:flex;flex-direction:column;gap:4px;padding:15px 18px;
  border-radius:10px;border:1px solid var(--rule);background:var(--paper);
  text-decoration:none;transition:border-color var(--t-state) var(--ease),transform var(--t-press) var(--ease);
}
.nav-card:hover,.nav-card:focus-visible{border-color:var(--ink)}
.nav-card.next{text-align:right;align-items:flex-end}
.nav-card .dir{font:500 11.5px/1 var(--mono);text-transform:uppercase;letter-spacing:.06em;color:var(--mute)}
.nav-card .title{font:600 15.5px/1.3 var(--sans);color:var(--ink)}
.doc-footer{
  margin-top:28px;display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:10px;
  font-size:13px;color:var(--mute);
}
.doc-footer a{color:var(--mute);text-decoration:none;border-bottom:1px solid var(--rule)}
.doc-footer a:hover{color:var(--ink);border-bottom-color:var(--ink)}

/* Responsive */
@media (max-width:960px){
  header.top{padding:0 16px;gap:12px}
  header.top .menu-btn{display:inline-flex}
  header.top .hill-link,header.top .nav{display:none}
  aside.sidebar{
    position:fixed;top:0;left:0;bottom:0;z-index:40;width:min(304px,86vw);max-height:none;
    background:var(--cream);padding:16px 16px 32px;
    border-right:1px solid var(--rule);box-shadow:12px 0 36px rgba(17,17,15,.14);
    transform:translateX(-104%);transition:transform 240ms var(--ease);
  }
  aside.sidebar.open{transform:translateX(0)}
  aside.sidebar .drawer-head{display:flex}
  .backdrop.open{display:block}
  main.doc{padding:28px 20px 72px}
}
@media (max-width:640px){
  .pill-copy .lbl{display:none}
  .pill-copy{padding:7px 9px}
  .info-grid,.doc-pager{grid-template-columns:1fr}
  main.doc{padding:22px 16px 64px}
  .code-block pre{font-size:12.5px;padding:12px 0}
  .code-block pre code{padding:0 14px}
  .code-block .ln.no{width:calc(100% + 28px);margin:0 -14px;padding:0 14px 0 11px}
}
@media (prefers-reduced-motion:reduce){
  html{scroll-behavior:auto}
  aside.sidebar,.pill-copy,.nav-card,header.top .nav a::after{transition:none}
}
"""

DOCS_JS = """
const AGENT_TXT = [
  'Hirsel docs and install for coding agents:',
  'Authority rule: Always run `hirsel primer` from your installed binary as the exact authority for your installed version.',
  '- Index: https://hirsel.ai/llms.txt',
  '- Full single-file bundle: https://hirsel.ai/llms-full.txt',
  '- Primer (whole language in one page): https://hirsel.ai/primer.md',
  '- Cookbook (tested recipes): https://hirsel.ai/cookbook.md',
  '- Standard library index: https://hirsel.ai/api-index.md',
  '- Agent skill: https://github.com/SamGalanakis/hirsel/blob/main/skills/hirsel/SKILL.md',
  '- Install (Linux x86_64, glibc 2.28+; Apple Silicon macOS in progress): curl -fsSL https://hirsel.ai/install.sh | sh',
].join('\\n');

function writeClip(text, done) {
  const fallback = () => {
    const t = document.createElement('textarea');
    t.value = text; t.setAttribute('readonly', '');
    t.style.cssText = 'position:fixed;opacity:0;pointer-events:none';
    document.body.append(t); t.select();
    try { if (document.execCommand('copy')) done(); } finally { t.remove(); }
  };
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done, fallback);
  } else fallback();
}

const copyAgent = document.getElementById('copyAgent');
if (copyAgent) {
  let timer = 0;
  copyAgent.addEventListener('click', () => {
    writeClip(AGENT_TXT, () => {
      copyAgent.classList.add('ok');
      clearTimeout(timer);
      timer = setTimeout(() => copyAgent.classList.remove('ok'), 1800);
    });
  });
}

document.querySelectorAll('.code-copy').forEach(btn => {
  let timer = 0;
  btn.addEventListener('click', () => {
    const block = btn.closest('.code-block');
    const code = block ? block.querySelector('pre code') : null;
    if (!code) return;
    writeClip(code.innerText.replace(/\\n$/, ''), () => {
      btn.classList.add('ok');
      const txt = btn.querySelector('.txt');
      if (txt) txt.textContent = 'Copied';
      clearTimeout(timer);
      timer = setTimeout(() => {
        btn.classList.remove('ok');
        if (txt) txt.textContent = 'Copy';
      }, 1600);
    });
  });
});

// Mobile sidebar drawer
const sidebar = document.getElementById('sidebar');
const backdrop = document.getElementById('backdrop');
const menuBtn = document.getElementById('menuBtn');
const closeBtn = document.getElementById('drawerClose');
function setDrawer(open) {
  if (!sidebar || !backdrop || !menuBtn) return;
  sidebar.classList.toggle('open', open);
  backdrop.classList.toggle('open', open);
  menuBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
}
if (menuBtn) menuBtn.addEventListener('click', () => setDrawer(!sidebar.classList.contains('open')));
if (closeBtn) closeBtn.addEventListener('click', () => setDrawer(false));
if (backdrop) backdrop.addEventListener('click', () => setDrawer(false));
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') setDrawer(false);
});
if (sidebar) {
  sidebar.querySelectorAll('a').forEach(a => a.addEventListener('click', () => setDrawer(false)));
}

// Scroll-spy for the sidebar: the active entry is the last part or section heading above the top bar.
const tocLinks = Array.from(document.querySelectorAll('#sidebar a[href^="#"]'));
const spy = tocLinks.map(a => [document.getElementById(a.getAttribute('href').slice(1)), a]).filter(p => p[0]);
if (spy.length) {
  let current = null, queued = false;
  const update = () => {
    queued = false;
    let next = spy[0][1];
    for (const [el, a] of spy) {
      if (el.getBoundingClientRect().top <= 96) next = a; else break;
    }
    if (next === current) return;
    if (current) current.classList.remove('active');
    current = next;
    current.classList.add('active');
    // Keep the active entry in view inside the scrolling sidebar.
    const r = current.getBoundingClientRect(), box = sidebar.getBoundingClientRect();
    if (r.top < box.top + 12 || r.bottom > box.bottom - 12) sidebar.scrollTop += r.top - box.top - box.height / 3;
  };
  addEventListener('scroll', () => { if (!queued) { queued = true; requestAnimationFrame(update); } }, { passive: true });
  addEventListener('resize', update);
  // A load with a #fragment jumps after this script runs and may fire no scroll event.
  addEventListener('load', update);
  addEventListener('hashchange', update);
  update();
}

// Instant filter on /docs/std/
const stdInput = document.getElementById('stdSearch');
if (stdInput) {
  const rows = Array.from(document.querySelectorAll('#stdTable tbody tr[data-mod]'));
  const countEl = document.getElementById('stdCount');
  const btns = Array.from(document.querySelectorAll('.std-filters .pill-btn'));
  let mode = 'all';
  const apply = () => {
    const q = stdInput.value.trim().toLowerCase();
    let shown = 0;
    for (const r of rows) {
      const extOk = mode === 'all' || r.dataset.ext === 'yes';
      const qOk = !q || r.dataset.search.includes(q);
      const show = extOk && qOk;
      r.hidden = !show;
      if (show) shown++;
    }
    if (countEl) countEl.textContent = shown + ' of ' + rows.length + ' modules shown';
  };
  stdInput.addEventListener('input', apply);
  btns.forEach(b => b.addEventListener('click', () => {
    mode = b.dataset.filter;
    btns.forEach(x => x.setAttribute('aria-pressed', x === b ? 'true' : 'false'));
    apply();
  }));
  document.addEventListener('keydown', e => {
    if (e.key === '/' && document.activeElement !== stdInput && !/INPUT|TEXTAREA/.test(document.activeElement.tagName)) {
      e.preventDefault();
      stdInput.focus();
    }
  });
}
"""


def render_sidebar(
    groups: list[tuple[str, str, list[tuple[str, str]]]],
    *,
    on_docs: bool,
    std_toc: list[tuple[str, str]],
    ref: str,
) -> str:
    """Sidebar for both pages: the /docs/ parts (Install, Language, Cookbook) with their
    sections when on /docs/, then the reference links (standard library, spec)."""
    spec_url = f"{GITHUB_REPO}/tree/{ref}/docs/spec"

    def sub(items: list[tuple[str, str]]) -> str:
        lis = "".join(
            f'<li><a href="#{html.escape(anchor, quote=True)}">{text_html}</a></li>'
            for anchor, text_html in items
        )
        return f'<ul class="toc-sub">{lis}</ul>'

    if on_docs:
        parts = "".join(
            f'<div class="side-group"><a class="side-label" href="#{pid}">{label}</a>{sub(items)}</div>'
            for label, pid, items in groups
        )
        std_entry = '<li><a class="side-page" href="/docs/std/"><span>Standard library</span><span class="tag">std</span></a></li>'
    else:
        links = "".join(
            f'<li><a class="side-page" href="/docs/#{pid}"><span>{label}</span></a></li>'
            for label, pid, _items in groups
        )
        parts = f'<div class="side-group"><p class="side-label">Docs</p><ul class="side-list">{links}</ul></div>'
        std_entry = (
            '<li><a class="side-page" href="/docs/std/" aria-current="page"><span>Standard library</span>'
            f'<span class="tag">std</span></a>{sub(std_toc)}</li>'
        )
    return (
        f'<aside class="sidebar" id="sidebar" aria-label="Documentation navigation">'
        f'<div class="drawer-head"><span class="wm wmfont">hirsel docs</span>'
        f'<button class="drawer-close" id="drawerClose" type="button" aria-label="Close navigation">{CLOSE_ICON}</button></div>'
        f"{parts}"
        f'<div class="side-group"><p class="side-label">Reference</p><ul class="side-list">{std_entry}'
        f'<li><a class="side-page" href="{html.escape(spec_url, quote=True)}"><span>Language spec</span><span class="tag">&#8599;</span></a></li>'
        f"</ul></div>"
        f'<div class="side-foot"><a href="/"><span>&larr; Take a walk</span><span class="sub">The hill</span></a></div>'
        f"</aside>"
    )


def render_page(
    *,
    slug: str,
    title: str,
    description: str,
    eyebrow: str,
    heading: str,
    lead: str,
    body_html: str,
    nav_key: str,
    sidebar_html: str,
    pager: tuple[tuple[str, str] | None, tuple[str, str] | None],
    ref: str,
    markdown_alternates: list[str],
) -> str:
    canonical = f"{ORIGIN}{slug}"
    crumb_items = [
        {"@type": "ListItem", "position": 1, "name": "Hirsel", "item": f"{ORIGIN}/"},
        {"@type": "ListItem", "position": 2, "name": "Docs", "item": f"{ORIGIN}/docs/"},
    ]
    if slug != "/docs/":
        crumb_items.append(
            {"@type": "ListItem", "position": 3, "name": heading, "item": canonical}
        )
    jsonld = {
        "@context": "https://schema.org",
        "@graph": [
            {
                "@type": "TechArticle",
                "headline": title,
                "description": description,
                "url": canonical,
                "inLanguage": "en",
                "image": f"{ORIGIN}/og-image.png",
                "isPartOf": {"@id": f"{ORIGIN}/#website"},
                "about": {"@id": f"{ORIGIN}/#language"},
            },
            {
                "@type": "BreadcrumbList",
                "itemListElement": crumb_items,
            },
        ],
    }

    alt_links = [
        f'  <link rel="alternate" type="text/markdown" href="{href}">'
        for href in ["/llms.txt", *markdown_alternates]
    ]

    top_nav_html = "".join(
        f'<a href="{href}"' + (' aria-current="page"' if key == nav_key else "") + f">{label}</a>"
        for key, href, label in (("docs", "/docs/", "Docs"), ("std", "/docs/std/", "Standard library"))
    )

    prev_item, next_item = pager
    pager_cards = []
    if prev_item:
        pager_cards.append(
            f'<a class="nav-card prev" href="{prev_item[0]}">'
            f'<span class="dir">&larr; Previous</span>'
            f'<span class="title">{html.escape(prev_item[1])}</span></a>'
        )
    else:
        pager_cards.append("<div></div>")
    if next_item:
        pager_cards.append(
            f'<a class="nav-card next" href="{next_item[0]}">'
            f'<span class="dir">Next &rarr;</span>'
            f'<span class="title">{html.escape(next_item[1])}</span></a>'
        )
    pager_html = f'<nav class="doc-pager" aria-label="Page navigation">{"".join(pager_cards)}</nav>'

    ref_note = f"Aligned with {ref}" if ref != "main" else "Tracking main"
    footer_html = (
        f'<footer class="doc-footer">'
        f'<span>Hirsel &middot; {html.escape(ref_note)} &middot; Run <code>hirsel primer</code> from your binary for exact local authority.</span>'
        f'<span><a href="/">Take a walk</a> &middot; <a href="{GITHUB_REPO}">GitHub</a></span>'
        f'</footer>'
    )

    escaped_title = html.escape(title, quote=True)
    escaped_desc = html.escape(description, quote=True)

    return "\n".join(
        [
            "<!doctype html>",
            AGENT_COMMENT,
            '<html lang="en">',
            "<head>",
            '<meta charset="utf-8">',
            '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">',
            f"  <title>{html.escape(title)}</title>",
            f'  <meta name="description" content="{escaped_desc}">',
            '  <meta name="theme-color" content="#FBF6EC">',
            '  <link rel="icon" href="/favicon.ico" sizes="any">',
            '  <link rel="icon" href="/favicon.svg" type="image/svg+xml">',
            '  <link rel="apple-touch-icon" href="/apple-touch-icon.png">',
            f'  <link rel="canonical" href="{canonical}">',
            *alt_links,
            '  <meta property="og:type" content="article">',
            '  <meta property="og:site_name" content="Hirsel">',
            f'  <meta property="og:title" content="{escaped_title}">',
            f'  <meta property="og:description" content="{escaped_desc}">',
            f'  <meta property="og:url" content="{canonical}">',
            f'  <meta property="og:image" content="{ORIGIN}/og-image.png">',
            '  <meta property="og:image:width" content="1200">',
            '  <meta property="og:image:height" content="630">',
            '  <meta name="twitter:card" content="summary_large_image">',
            f'  <meta name="twitter:title" content="{escaped_title}">',
            f'  <meta name="twitter:description" content="{escaped_desc}">',
            f'  <meta name="twitter:image" content="{ORIGIN}/og-image.png">',
            '  <script type="application/ld+json">',
            json.dumps(jsonld, indent=2, ensure_ascii=False),
            "  </script>",
            '  <link rel="preload" href="/fonts/archivo-black-latin.woff2" as="font" type="font/woff2" crossorigin>',
            '  <link rel="preload" href="/fonts/inter-latin.woff2" as="font" type="font/woff2" crossorigin>',
            "<style>",
            DOCS_CSS.strip(),
            "</style>",
            "</head>",
            "<body>",
            '<header class="top">',
            f'  <button class="menu-btn" id="menuBtn" type="button" aria-label="Open navigation" aria-controls="sidebar" aria-expanded="false">{MENU_ICON}</button>',
            f'  <a class="brand" href="/" aria-label="Hirsel home">{RAM_MARK_SVG}<span class="wm wmfont">hirsel</span></a>',
            f'  <nav class="nav" aria-label="Documentation">{top_nav_html}</nav>',
            '  <div class="right">',
            '    <a class="hill-link" href="/"><span class="dot" aria-hidden="true"></span>Take a walk</a>',
            f'    <button class="pill-copy" id="copyAgent" type="button" title="Copy Hirsel docs and install links for coding agents" aria-live="polite">{COPY_ICON}{OK_ICON}<span class="lbl"><span class="idle">Copy for agents</span><span class="done" aria-hidden="true">Copied</span></span></button>',
            f'    <a class="gh" href="{GITHUB_REPO}" aria-label="Source on GitHub" title="Source on GitHub">{GITHUB_ICON}</a>',
            "  </div>",
            "</header>",
            '<div class="backdrop" id="backdrop"></div>',
            '<div class="shell">',
            sidebar_html,
            '<main class="doc">',
            '  <div class="hero-doc">',
            f"    {TOPO_ACCENT_SVG}",
            f'    <p class="eyebrow"><span class="pip" aria-hidden="true"></span>{html.escape(eyebrow)}</p>',
            f"    <h1>{html.escape(heading)}</h1>",
            f'    <p class="lead">{lead}</p>',
            "  </div>",
            body_html,
            pager_html,
            footer_html,
            "</main>",
            "</div>",
            "<script>",
            DOCS_JS.strip(),
            "</script>",
            "</body>",
            "</html>",
            "",
        ]
    )
