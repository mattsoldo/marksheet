import "./styles.css";
import budget from "../../examples/budget.ms?raw";
import invoice from "../../examples/invoice-basic.ms?raw";
import census from "../../examples/census-population.ms?raw";
import stamps from "../../examples/stamp-catalog.ms?raw";
import tables from "../../examples/excel-tables.ms?raw";
import dates from "../../examples/dates-1904.ms?raw";
import { highlightLines } from "./highlight";
import { Playground } from "./playground";

const EXAMPLES = [
  { file: "budget.ms", label: "Household budget", source: budget },
  { file: "invoice-basic.ms", label: "Invoice", source: invoice },
  { file: "census-population.ms", label: "Census population", source: census },
  { file: "stamp-catalog.ms", label: "Stamp catalog", source: stamps },
  { file: "excel-tables.ms", label: "Excel tables (CSV rows)", source: tables },
  { file: "dates-1904.ms", label: "1904 dates (CSV rows)", source: dates },
] as const;

function startPlayground(): void {
  const root = document.querySelector<HTMLElement>("[data-playground]");
  const picker = root?.querySelector<HTMLSelectElement>("[data-examples]");
  const fileName = root?.querySelector<HTMLElement>("[data-file-name]");
  const editor = root?.querySelector<HTMLTextAreaElement>("[data-editor]");
  if (!root || !picker || !fileName || !editor) return;

  picker.replaceChildren(...EXAMPLES.map((example, index) => new Option(example.label, String(index))));
  editor.value = EXAMPLES[0].source;
  const playground = new Playground(root);
  picker.addEventListener("change", () => {
    const example = EXAMPLES[Number(picker.value)];
    if (!example) return;
    fileName.textContent = example.file;
    playground.load(example.source);
  });
  playground.start();
}

/** Static samples share the playground's highlighter. */
function highlightSnippets(): void {
  for (const block of document.querySelectorAll<HTMLElement>("[data-ms]")) {
    const code = document.createElement("code");
    code.innerHTML = highlightLines(block.textContent ?? "").join("\n");
    block.replaceChildren(code);
  }
}

function trackHeader(): void {
  const header = document.querySelector<HTMLElement>(".site-header");
  if (!header) return;
  const update = () => header.classList.toggle("is-scrolled", window.scrollY > 8);
  window.addEventListener("scroll", update, { passive: true });
  update();
}

highlightSnippets();
trackHeader();
startPlayground();
