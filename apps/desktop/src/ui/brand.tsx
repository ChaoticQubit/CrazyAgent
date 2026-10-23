const COIL = "M59 5H5V59H59V15H15V49H49V22";
const SQUARES = [
  ["21.85", "25.05", "9.5", "rotate(24 26.6 29.8)"],
  ["33.75", "21.05", "6.5", "rotate(-16 37 24.3)"],
  ["32.8", "31.1", "7", "rotate(39 36.3 34.6)"],
  ["26.55", "37.75", "5.5", "rotate(-11 29.3 40.5)"],
] as const;
const REDUCED_COIL = "M15 1H1V15H15V4H4V13";
const REDUCED_SQUARES = [
  ["6.4", "8.8", "3.8", "rotate(45 8.3 10.7)"],
  ["10.3", "5.9", "2.8", "rotate(18 11.7 7.3)"],
] as const;

function squares(list: readonly (readonly [string, string, string, string])[]) {
  return list.map(([x, y, side, transform]) =>
    <rect key={transform} x={x} y={y} width={side} height={side} transform={transform} className="brand-square" />);
}

/** The CrazyAgent brand: the Ember 4 mark (an ink coil holding four vermilion squares) and the
 * name. The coil takes the text color and the squares the brand token, so it follows light and
 * dark. Screens under 2x show the reduced drawing in the sidebar, because the full mark needs 48
 * device pixels. Input: large for the first-run page. Output: an image named "CrazyAgent". */
export function BrandMark({ large = false }: { large?: boolean }) {
  const size = large ? 48 : 24;
  return <span className={`brand-mark${large ? " large" : ""}`} role="img" aria-label="CrazyAgent">
    <svg className="brand-glyph full" width={size} height={size} viewBox="0 0 64 64" focusable="false" aria-hidden="true">
      <path d={COIL} fill="none" stroke="currentColor" strokeWidth={6} strokeLinejoin="miter" strokeLinecap="butt" />
      {squares(SQUARES)}
    </svg>
    {large ? null : <svg className="brand-glyph reduced" width={size} height={size} viewBox="0 0 16 16" focusable="false" aria-hidden="true">
      <path d={REDUCED_COIL} fill="none" stroke="currentColor" strokeWidth={2} strokeLinejoin="miter" strokeLinecap="butt" />
      {squares(REDUCED_SQUARES)}
    </svg>}
    <span className="brand-name">CrazyAgent</span>
  </span>;
}
