// Monaco, bundled locally (no CDN: the CSP is default-src 'self'). Workers come
// from Vite's ?worker imports so they are real files in the build, not blobs.
import * as monaco from "monaco-editor";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";
import { THEMES, watchTheme } from "./themes";

self.MonacoEnvironment = {
  getWorker(_: string, label: string) {
    if (label === "json") return new jsonWorker();
    if (label === "css" || label === "scss" || label === "less") return new cssWorker();
    if (label === "html" || label === "handlebars" || label === "razor") return new htmlWorker();
    if (label === "typescript" || label === "javascript") return new tsWorker();
    return new editorWorker();
  },
};

// One Monaco theme per app theme (ids match themes.ts); follow the page's theme.
for (const t of Object.values(THEMES)) {
  monaco.editor.defineTheme(t.id, { base: t.editor.base, inherit: true, rules: [], colors: t.editor.colors });
}
watchTheme((t) => monaco.editor.setTheme(t.id));

export { monaco };

/** Language id for a file name, from Monaco's own extension registry. */
export function languageFor(name: string): string {
  const lower = name.toLowerCase();
  const ext = lower.includes(".") ? lower.slice(lower.lastIndexOf(".")) : "";
  for (const l of monaco.languages.getLanguages()) {
    if (l.filenames?.some((f) => f.toLowerCase() === lower)) return l.id;
    if (ext && l.extensions?.some((e) => e.toLowerCase() === ext)) return l.id;
  }
  // Shell-ish files without an extension that are common on servers.
  if (/^(dockerfile)$/i.test(lower)) return "dockerfile";
  if (/^(\.?bashrc|\.?profile|\.?zshrc|\.?bash_profile)$/i.test(lower)) return "shell";
  return "plaintext";
}

export function languageIds(): { id: string; label: string }[] {
  return monaco.languages
    .getLanguages()
    .map((l) => ({ id: l.id, label: l.aliases?.[0] ?? l.id }))
    .sort((a, b) => a.label.localeCompare(b.label));
}

// ---------------------------------------------------------------- snippets & formatting

interface Snippet { label: string; body: string; doc?: string }

const SNIPPETS: Record<string, Snippet[]> = {
  shell: [
    { label: "shebang", body: "#!/usr/bin/env bash\nset -euo pipefail\n\n$0", doc: "bash header + strict mode" },
    { label: "if", body: "if [[ ${1:condition} ]]; then\n\t$0\nfi" },
    { label: "for", body: "for ${1:x} in ${2:list}; do\n\t$0\ndone" },
    { label: "while", body: "while ${1:condition}; do\n\t$0\ndone" },
    { label: "case", body: "case ${1:word} in\n\t${2:pattern}) $0 ;;\n\t*) ;;\nesac" },
    { label: "func", body: "${1:name}() {\n\t$0\n}" },
  ],
  python: [
    { label: "def", body: "def ${1:name}(${2:args}):\n\t${0:pass}" },
    { label: "class", body: "class ${1:Name}:\n\tdef __init__(self${2:, args}):\n\t\t$0" },
    { label: "main", body: "if __name__ == \"__main__\":\n\t${0:main()}" },
    { label: "for", body: "for ${1:x} in ${2:iterable}:\n\t$0" },
    { label: "try", body: "try:\n\t${1:pass}\nexcept ${2:Exception} as e:\n\t$0" },
    { label: "with", body: "with ${1:open(${2:path})} as ${3:f}:\n\t$0" },
  ],
  yaml: [
    { label: "doc", body: "---\n$0", doc: "document separator" },
    { label: "map", body: "${1:key}:\n\t${2:child}: ${3:value}\n$0" },
    { label: "seq", body: "${1:key}:\n\t- ${2:item}\n$0" },
  ],
  ini: [
    { label: "section", body: "[${1:section}]\n${2:key} = ${3:value}\n$0" },
    { label: "kv", body: "${1:key} = ${2:value}" },
  ],
};

let registered = false;

/** Snippet completions and light formatters for languages Monaco has no LSP for. */
export function registerEditorExtras(): void {
  if (registered) return;
  registered = true;

  for (const [lang, snips] of Object.entries(SNIPPETS)) {
    monaco.languages.registerCompletionItemProvider(lang, {
      provideCompletionItems(model, position) {
        const word = model.getWordUntilPosition(position);
        const range = new monaco.Range(position.lineNumber, word.startColumn, position.lineNumber, word.endColumn);
        return {
          suggestions: snips.map((s) => ({
            label: s.label,
            kind: monaco.languages.CompletionItemKind.Snippet,
            insertText: s.body,
            insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
            documentation: s.doc ?? s.body,
            detail: "charon snippet",
            range,
          })),
        };
      },
    });
  }

  // JSON has a real formatter already; give YAML/Markdown a tidy pass (trailing
  // whitespace, ≤1 blank line between blocks, final newline).
  const tidy = (text: string): string =>
    text.replace(/[ \t]+$/gm, "").replace(/\n{3,}/g, "\n\n").replace(/\s*$/, "\n");
  for (const lang of ["yaml", "markdown"]) {
    monaco.languages.registerDocumentFormattingEditProvider(lang, {
      provideDocumentFormattingEdits(model) {
        return [{ range: model.getFullModelRange(), text: tidy(model.getValue()) }];
      },
    });
  }
}
