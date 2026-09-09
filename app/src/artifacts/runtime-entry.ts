import * as Solid from "solid-js";
import * as Web from "@solidjs/web";
// Bundled as source text at build time, executed only inside the opaque iframe.
Object.assign(globalThis, { __artifactModules: { "solid-js": Solid, "@solidjs/web": Web } });
