import "./compiler-environment";
import { transform } from "@babel/standalone";
import solid from "@solidjs/babel-plugin";

export function compileArtifact(source: string): string {
  if (source.length > 1_000_000) throw new Error("Artifact source is too large to preview.");
  return transform(source, { filename: "artifact.jsx", sourceType: "module", plugins: [[solid, { generate: "dom", moduleName: "@solidjs/web", hydratable: false }], "transform-modules-commonjs"] }).code ?? "";
}
