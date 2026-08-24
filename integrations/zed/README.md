# Zed preview integration

Zed does not currently expose an extension API for a custom rendered-document
pane or embedded webview. This project therefore provides the supported,
working preview integration: a project task that builds the self-contained
Marksheet viewer, serves it on `127.0.0.1:4173`, and opens it in the system
browser.

1. Open this repository as a Zed worktree.
2. Open the Command Palette and run **task: spawn**.
3. Choose **Preview Marksheet viewer**.
4. In the browser viewer, choose the `.ms` file to preview.

The task is defined in [`.zed/tasks.json`](../../.zed/tasks.json) and runs
[`viewer/scripts/zed-preview.sh`](../../viewer/scripts/zed-preview.sh). The
task stays active while the preview server is running; cancel the task to stop
the server. It does not publish, upload, or modify the selected workbook.

For a future in-Zed rendered preview, Zed would need to add a preview or
webview extension capability. Until then, this task keeps the preview flow
usable without pretending that a language extension can render the viewer.
