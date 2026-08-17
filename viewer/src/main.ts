import "./styles.css";
import { ViewerApp } from "./app";
import { createBindingAdapter } from "./worker-adapter";

async function main(): Promise<void> {
  const root = document.querySelector<HTMLElement>("#app");
  if (!root) throw new Error("missing viewer root");

  try {
    const adapter = createBindingAdapter();
    const app = new ViewerApp(root, adapter);
    window.addEventListener("beforeunload", () => app.dispose(), { once: true });
  } catch (error) {
    root.innerHTML = `<div class="empty-state"><div><h1>Marksheet Viewer</h1><p id="startup-error"></p></div></div>`;
    const message = root.querySelector("#startup-error");
    if (message) {
      message.textContent = `The worker binding could not be loaded: ${error instanceof Error ? error.message : String(error)}`;
    }
  }
}

void main();
