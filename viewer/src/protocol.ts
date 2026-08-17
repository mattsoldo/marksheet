/**
 * Viewer aliases for the generated `marksheet-worker@1` declaration surface.
 * Keeping one source of wire truth prevents the browser shell from drifting as
 * bounded projection records evolve.
 */
export type {
  A1Range,
  AxisGeometry,
  ByteSpan,
  CalculationResult,
  CellError,
  Diagnostic,
  EditOperation,
  EditTransaction,
  ExtensionDeclarationSummary,
  ExtensionInstanceSummary,
  ExtensionSupportSummary,
  NameSummary,
  NameTarget,
  NumberFormat,
  PresentedCell,
  ResolvedStyle,
  ScalarValue,
  SourcePatch,
  StyledRegion,
  StyleProperties,
  Value as AuthoredValue,
  VerticalAlignment,
  ViewCompleteness,
  VisibleRegion,
  WorkbookSnapshot,
  WorkerError as WorkerErrorShape,
  WorkerRequestEnvelope,
  WorkerResponseEnvelope,
} from "../../bindings/wasm/protocol.d.ts";

export type { A1Coordinate as Coordinate } from "../../bindings/wasm/protocol.d.ts";

export const PROTOCOL_VERSION = "marksheet-worker@1" as const;
