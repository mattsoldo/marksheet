# Marksheet Product Specification

**Status:** Draft 0.1

**Product:** Marksheet

**Tagline:** Markdown for spreadsheets

## 1. Elevator pitch

Marksheet is a plain-text spreadsheet format for people, coding agents, and
applications.

It gives spreadsheet workbooks the qualities that made Markdown successful for
documents: a source representation that is easy to generate, portable between
tools, durable over time, and pleasant to review in Git. A Marksheet file can be
opened as text or rendered as a familiar spreadsheet grid without requiring
Excel, Google Sheets, or a proprietary service.

## 2. The problem

Coding agents increasingly use Markdown as the common language for notes,
documentation, plans, and specifications. Markdown succeeds because its source
is simple, useful without a renderer, and supported by a broad ecosystem.

Spreadsheet data has no equivalent default:

- XLSX is capable but packaged, difficult to diff, and awkward when Excel is
  unavailable.
- Cloud spreadsheets depend on a particular service and conversion workflow.
- CSV is portable but represents only one flat table. It has no workbook,
  formulas, references, names, or presentation.
- Markdown tables are readable for small datasets but do not scale naturally
  to large or sparse sheets, formulas, or multiple worksheets.
- Existing open spreadsheet formats optimize for complete office-suite
  fidelity rather than concise source authoring.

As a result, agents either create opaque files, flatten the problem into tables,
or invent an ad hoc format for each task.

## 3. Product promise

A person or agent should be able to write a useful workbook using a text editor
and a small set of obvious constructs. That same file should open in a GUI that
feels like a conventional spreadsheet and should produce focused, meaningful
Git diffs when changed.

Marksheet provides:

- one versioned UTF-8 text file per workbook;
- any number of sheets;
- sparse placement of rectangular data blocks at A1 coordinates;
- named tables and named ranges;
- formulas and cross-sheet references;
- basic formatting and sheet geometry;
- a safe, declarative extension mechanism;
- deterministic parsing, calculation, and serialization rules; and
- lossless round trips through conforming editors.

## 4. Who it is for

### 4.1 Software

Software—including coding agents, scripts, libraries, converters, and
applications—needs a spreadsheet representation it can create, inspect, and
modify with ordinary file operations. It should not need to automate a desktop
application, depend on a cloud service, or reverse engineer a ZIP/XML package
to change a few cells.

Marksheet gives software a small, deterministic grammar and a stable workbook
model for generating data, resolving references, calculating formulas,
validating content, rendering grids, converting formats, and applying focused
source edits. Independent implementations can interoperate through the text
format and conformance corpus rather than a particular vendor SDK.

### 4.2 Humans

Humans need spreadsheet information to remain accessible, understandable, and
reviewable with the tools they already use. A person can inspect or repair a
Marksheet workbook in a text editor, collaborate through normal Git branching
and review workflows, or open the same source in a familiar spreadsheet GUI.

People who prefer a grid do not need to learn the source syntax. People who
prefer source can see values, formulas, names, and meaningful diffs without
opening a specialized application. Both experiences operate on the same
authoritative file.

## 5. Design principles

### 5.1 Source is the product

The text file is authoritative. A GUI is a view and editor for that source, not
the owner of a hidden richer representation.

### 5.2 Useful without special software

A text editor must be enough to inspect, author, repair, and review a workbook.
Rendering improves the experience but is not required to access the data.

### 5.3 Familiar spreadsheet semantics

Marksheet uses sheets, A1 addresses, ranges, formulas, and tables because those
concepts are already understood. Novel syntax must earn its cost.

### 5.4 Git friendliness is a hard requirement

Small logical changes should normally produce small textual changes. Canonical
serialization must be deterministic. Implementations must not reorder or
rewrite unrelated content as a side effect of an edit.

### 5.5 Agent friendliness is a hard requirement

The common path should use short, regular, locally understandable constructs.
An agent should not need a large schema, generated IDs for every cell, or a
binary SDK to make a valid workbook.

### 5.6 A small core and a large ecosystem

The core contains the features required to describe a recognizable spreadsheet.
Features that can be omitted without losing the workbook's primary values and
calculations belong in extensions.

### 5.7 Safe by default

Core formulas are pure and deterministic. Core workbooks contain no macros,
embedded executable code, network requests, or automatic plugin installation.

### 5.8 Honest interoperability

Importers and exporters must report unsupported or lossy features. They must
not silently discard workbook content and claim a faithful conversion.

## 6. The minimal core

The core consists of:

1. A format and formula-profile version.
2. Workbook metadata and deterministic calculation settings.
3. Ordered sheets with stable machine IDs and human labels.
4. Sparse, A1-anchored CSV blocks.
5. Named tables with headers and structured references.
6. Scalar values, including numbers, text, booleans, dates, and datetimes.
7. A portable formula language with cell, range, sheet, table, and name
   references.
8. Workbook-scoped named cells and ranges.
9. Reusable basic styles and range application.
10. Row heights and column widths.
11. Comments, deterministic serialization, and version compatibility rules.
12. Declarations and opaque payloads for extensions.

Named ranges and formulas are deliberately part of the core. Compare:

```text
=SUM(inputs!B5:B16)*(1-inputs!B2)
```

with:

```text
=SUM(monthly_revenue)*(1-tax_rate)
```

The second expression communicates intent to humans and agents, survives more
structural edits, and creates a better review surface.

## 7. Extensions

The following are valuable, but do not belong in the initial core:

- validation and input constraints;
- assertions and test cases;
- schemas, column types, and primary keys;
- input/calculated/output roles;
- charts and dashboards;
- conditional formatting;
- merged cells and advanced layout;
- comments attached to individual cells;
- external data connections;
- domain-specific formula functions;
- iterative calculation;
- saved views, filters, and pivots; and
- import/export profiles for particular applications.

Extensions are declarative data. A workbook can declare an extension optional
or required, but it cannot fetch or execute implementation code. Unknown
extension payloads remain in the source and survive a lossless edit.

## 8. Primary experiences

### 8.1 Create with an agent

1. The user describes a workbook.
2. The agent writes one `.ms` file.
3. A linter reports precise source locations for any problems.
4. The user opens the same file as a spreadsheet or reviews it as text.

### 8.2 Edit in a spreadsheet GUI

1. The user opens a Marksheet file.
2. The application renders a virtualized grid for each sheet.
3. The user edits cells, formulas, names, and styles normally.
4. The editor changes only the corresponding source tokens or blocks.
5. Git shows the logical edit rather than a regenerated workbook package.

### 8.3 Review a change

1. A pull request displays ordinary text diffs.
2. Reviewers can see changed values and formulas directly.
3. An optional semantic diff explains moved ranges, renamed sheets, and formula
   effects without replacing the source diff.

### 8.4 Convert to or from another spreadsheet

1. A converter maps supported workbook features.
2. It emits a machine-readable conversion report.
3. Unsupported features are preserved when possible and otherwise identified
   explicitly.

### 8.5 Use from a coding harness

1. A coding agent loads the portable Marksheet skill or discovers local
   Marksheet tools.
2. The agent can read the concise authoring guide and examples without loading
   the complete specification into its context.
3. It creates or edits `.ms` source directly for ordinary work and uses the
   CLI's structured interface for validation, calculation, queries, and safe
   source-aware edits.
4. Harnesses with tool protocols may use a local Marksheet tool server backed
   by the same core library.
5. The resulting workbook remains ordinary, portable Marksheet source with no
   harness-specific data embedded in it.

## 9. GUI expectations

A full Marksheet editor should be able to provide:

- a sheet tab for every `@sheet` section;
- an effectively unbounded, virtualized grid;
- a formula bar displaying source formulas;
- a name box for addresses and named ranges;
- familiar table headers and structured formulas;
- number, font, color, alignment, wrapping, row-height, and column-width
  controls;
- visible diagnostics for unsupported required extensions; and
- source-aware undo, redo, and save.

The core does not prescribe a visual design. It prescribes enough semantics for
independent applications to render materially equivalent workbooks.

## 10. Git experience requirements

A conforming editor should:

- preserve comments, blank-line grouping, unknown extensions, and unaffected
  source spelling where possible;
- retain stable sheet, table, style, and name identifiers;
- edit a cell in place instead of serializing the entire workbook;
- avoid volatile metadata such as save timestamps;
- use UTF-8, LF line endings, and a final newline in canonical output;
- expose a separate explicit canonical-format command; and
- never make canonical reformatting an unavoidable side effect of opening and
  saving a file.

Directory-mode workbooks may be explored later for very large datasets, but the
initial interchange unit is one file.

## 11. Non-goals for the core

Marksheet 1.0 does not aim to:

- reproduce every Excel or Google Sheets feature;
- guarantee pixel-identical rendering across applications;
- execute VBA, JavaScript, Python, shell commands, or arbitrary formulas;
- act as a database or collaborative synchronization protocol;
- define a cloud service, storage provider, or permissions system;
- make raw million-row files comfortable for a person to read line by line;
- replace CSV for applications that need only one flat table; or
- make every workbook conversion lossless.

## 12. Success criteria for 1.0

Marksheet is ready for a stable 1.0 when:

1. Two independent parsers produce the same workbook model for the conformance
   corpus.
2. Two independent calculators agree on every portable formula test.
3. A reference GUI can open, calculate, edit, and save all core features.
4. A single-cell GUI edit produces a focused source diff.
5. Unknown optional extensions survive a lossless edit byte-for-byte.
6. Unsupported required extensions produce a visible failure, not silent data
   loss.
7. At least one XLSX converter and one CSV converter publish conversion reports.
8. Large sparse sheets render without allocating every intervening cell.
9. The grammar and versioning policy have passed public implementation review.
10. A documented harness integration kit can create, inspect, calculate, and
    minimally edit workbooks through at least two coding-agent environments.

## 13. Delivery sequence

### Phase 1: Language

- Ratify the core grammar and workbook intermediate representation.
- Build parser, formatter, linter, and conformance fixtures.
- Establish the portable formula profile.

### Phase 2: Utility

- Build a calculation engine adapter.
- Build CSV and XLSX conversion tools.
- Publish language-server diagnostics and syntax highlighting.
- Publish the first portable agent skill and structured CLI interface.

### Phase 3: Experience

- Build a lightweight cross-platform viewer/editor.
- Add semantic diff and merge tooling.
- Stabilize extension APIs from real plugin implementations.
- Add an optional local tool server and tested adapters for coding harnesses.

## 14. Open design questions

Draft 0.1 intentionally leaves these decisions open for implementation feedback:

- whether `.ms` should remain the recommended extension despite its historical
  use by `groff` manuscript files;
- the exact breadth of the first portable formula-function set;
- the best lossless-edit representation and source-map API;
- thresholds and packaging for extremely large workbooks; and
- whether a future directory form should be a core packaging profile or an
  extension.
