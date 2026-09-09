import { compileArtifact } from "./compiler";
self.onmessage = (event: MessageEvent<string>) => {
  try {
    self.postMessage({ code: compileArtifact(event.data) });
  } catch (error) { self.postMessage({ error: error instanceof Error ? error.message.slice(0, 1200) : "Could not compile this artifact." }); }
};
