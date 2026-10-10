# Marksheet website

The project's landing page, with a live playground that runs the Rust
reference engine in the browser. The GitHub Pages build also serves the full
[viewer](../viewer/README.md) beneath `app/`.

The playground uses the same `marksheet-worker@1` client and Wasm build as the
viewer, and reuses the viewer's presentation code, so a cell looks the same in
both. Example workbooks are bundled from [`examples/`](../examples/README.md).

## Develop

Install the Rust target and `wasm-bindgen` CLI as described in the viewer
README, then:

```sh
cd site
npm install
npm run dev
```

`npm run dev` and `npm run build` stage the Wasm worker under the ignored
`public/marksheet-wasm/` directory with the viewer's `prepare-wasm.sh`.

## Build and test the Pages site

```sh
cd viewer && npm install && cd ../site
npm run build:pages
npm run test:e2e
```

`npm run build:pages` builds this page and the viewer with relative asset URLs
and assembles both in `dist/`, so the same output works at a domain root or a
project path such as `/marksheet/`. `npm run test:e2e` serves `dist/` and checks
the playground's calculation, editing, error handling and source links, the
viewer route, accessibility in light and dark mode, and the phone layout.

## Deployment

`.github/workflows/pages.yml` deploys `dist/` on every push to `main`. Enable it
once in the repository's **Settings → Pages** by choosing **GitHub Actions** as
the source.
