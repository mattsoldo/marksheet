import type {
  A1Range,
  AuthoredValue,
  Coordinate,
  PresentedCell,
  ResolvedStyle,
  ScalarValue,
  StyledRegion,
  StyleProperties,
} from "./protocol";

/** Formats without consulting the host locale or clock. */
export function formatPresentedCell(cell: PresentedCell | undefined, locale: string): string {
  if (!cell) return "";
  if (cell.calculated) return formatScalar(cell.calculated, cell.style.properties, locale);
  if ("VirtualFill" in cell.source) return cell.source.VirtualFill.formula;
  const authored = cell.source.Authored.value;
  if (authored.kind === "formula") return authored.value;
  if (authored.kind === "text" && authored.value === "") return '""';
  return formatScalar(authored, cell.style.properties, locale);
}

export function formatScalar(
  value: ScalarValue | AuthoredValue,
  properties: StyleProperties,
  locale: string,
): string {
  if (value.kind === "blank") return "";
  if (value.kind === "boolean") return value.value ? "true" : "false";
  if (value.kind !== "number") {
    if (normalizeEnum(properties.number) === "date" && value.kind === "date_time") {
      return value.value.slice(0, 10);
    }
    return String(value.value);
  }

  const format = normalizeEnum(properties.number) ?? "general";
  const decimals = boundedDecimals(properties.decimals);
  switch (format) {
    case "integer":
      return groupedFixed(value.value, 0);
    case "decimal":
      return groupedFixed(value.value, decimals ?? 2);
    case "percent":
      return `${groupedFixed(value.value * 100, decimals ?? 0)}%`;
    case "currency": {
      const code = typeof properties.currency === "string" ? properties.currency.toUpperCase() : "";
      const amount = groupedFixed(Math.abs(value.value), decimals ?? 2);
      const sign = value.value < 0 ? "-" : "";
      const symbol = locale === "en-US" ? currencySymbol(code) : undefined;
      return symbol ? `${sign}${symbol}${amount}` : `${sign}${code || "¤"} ${amount}`;
    }
    default:
      return String(value.value);
  }
}

/** Spreadsheet column units approximate the width of the `0` glyph. */
export function columnTrackCss(size: number | null | undefined): string {
  return validSize(size) ? `max(56px, ${cssNumber(size)}ch)` : "112px";
}

/** Marksheet row geometry and font size are authored in typographic points. */
export function rowHeightCss(size: number | null | undefined): string {
  return validSize(size) ? `max(22px, ${cssNumber(size)}pt)` : "28px";
}

export function applyResolvedStyle(
  element: HTMLElement,
  properties: StyleProperties,
  valueKind: ScalarValue["kind"] | AuthoredValue["kind"] | undefined,
): void {
  if (properties.bold === true) element.style.fontWeight = "700";
  if (properties.italic === true) element.style.fontStyle = "italic";
  if (typeof properties.text_color === "string") element.style.color = properties.text_color;
  if (typeof properties.fill === "string") element.style.backgroundColor = properties.fill;
  if (validSize(properties.font_size)) element.style.fontSize = `${cssNumber(properties.font_size)}pt`;

  const alignment = normalizeEnum(properties.align) ?? "general";
  const resolvedAlignment = alignment === "general"
    ? (valueKind === "number" || valueKind === "date" || valueKind === "date_time" ? "right" : "left")
    : alignment;
  element.style.textAlign = resolvedAlignment;
  element.style.justifyContent = resolvedAlignment === "right"
    ? "flex-end"
    : resolvedAlignment === "center"
      ? "center"
      : "flex-start";

  const vertical = normalizeEnum(properties.valign) ?? "middle";
  element.style.alignItems = vertical === "top"
    ? "flex-start"
    : vertical === "bottom"
      ? "flex-end"
      : "center";
  if (properties.wrap === true) element.style.whiteSpace = "normal";
}

export function presentedValueKind(cell: PresentedCell): ScalarValue["kind"] | AuthoredValue["kind"] {
  if (cell.calculated) return cell.calculated.kind;
  if ("Authored" in cell.source) return cell.source.Authored.value.kind;
  return "formula";
}

/** Resolves sparse style rectangles onto only the finite rendered viewport. */
export function buildViewportStyleMap(
  regions: StyledRegion[],
  viewport: A1Range,
): Map<string, ResolvedStyle> {
  const styles = new Map<string, ResolvedStyle>();
  for (const region of [...regions].sort((left, right) => left.source_order - right.source_order)) {
    const startColumn = Math.max(viewport.start.column, region.range.start.column);
    const endColumn = Math.min(viewport.end.column, region.range.end.column);
    const startRow = Math.max(viewport.start.row, region.range.start.row);
    const endRow = Math.min(viewport.end.row, region.range.end.row);
    if (startColumn > endColumn || startRow > endRow) continue;
    for (let row = startRow; row <= endRow; row += 1) {
      for (let column = startColumn; column <= endColumn; column += 1) {
        const key = coordinateKey({ column, row });
        const previous = styles.get(key) ?? emptyResolvedStyle();
        styles.set(key, mergeResolvedStyle(previous, region.style));
      }
    }
  }
  return styles;
}

export function emptyStyleProperties(): StyleProperties {
  return {
    bold: null,
    italic: null,
    wrap: null,
    text_color: null,
    fill: null,
    font_size: null,
    align: null,
    valign: null,
    number: null,
    decimals: null,
    currency: null,
  };
}

function emptyResolvedStyle(): ResolvedStyle {
  return { properties: emptyStyleProperties(), layers: [] };
}

function mergeResolvedStyle(base: ResolvedStyle, layer: ResolvedStyle): ResolvedStyle {
  const properties = { ...base.properties };
  for (const key of Object.keys(properties) as Array<keyof StyleProperties>) {
    const value = layer.properties[key];
    if (value !== null) Object.assign(properties, { [key]: value });
  }
  return { properties, layers: [...base.layers, ...layer.layers] };
}

function coordinateKey(coordinate: Coordinate): string {
  return `${coordinate.column}:${coordinate.row}`;
}

function normalizeEnum(value: unknown): string | undefined {
  return typeof value === "string" ? value.toLowerCase() : undefined;
}

function boundedDecimals(value: unknown): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 15
    ? value
    : undefined;
}

function groupedFixed(value: number, decimals: number): string {
  const [integer = "0", fraction] = Math.abs(value).toFixed(decimals).split(".");
  const grouped = integer.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const sign = value < 0 ? "-" : "";
  return fraction === undefined ? `${sign}${grouped}` : `${sign}${grouped}.${fraction}`;
}

function currencySymbol(code: string): string | undefined {
  return ({ USD: "$", EUR: "€", GBP: "£", JPY: "¥" } as Record<string, string>)[code];
}

function validSize(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function cssNumber(value: number): string {
  return String(value);
}
