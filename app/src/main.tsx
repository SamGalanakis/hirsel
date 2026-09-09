import "@fontsource-variable/inter";
import { render } from "@solidjs/web";
import App from "./App";
import { registerServiceWorker } from "./lib/pwa";
import "./styles.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("Root element not found");
}

render(() => <App />, root);

registerServiceWorker();
