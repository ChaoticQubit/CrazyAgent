import { useState, type ReactNode } from "react";

/** Works out which rows of a fixed-height list are visible, plus overscan on both sides.
 * Inputs: scroll offset, viewport height, row height, row count, overscan rows.
 * Output: the first row index and one past the last. */
export function windowRange(scrollTop: number, viewport: number, rowHeight: number, count: number, overscan: number): { start: number; end: number } {
  if (!(rowHeight > 0) || count <= 0) return { start: 0, end: 0 };
  const top = Math.max(0, scrollTop);
  const start = Math.max(0, Math.floor(top / rowHeight) - overscan);
  const end = Math.min(count, Math.ceil((top + Math.max(0, viewport)) / rowHeight) + overscan);
  return { start, end: Math.max(start, end) };
}

/** A fixed-row-height list that renders only visible rows plus overscan, so thousands of
 * accounts stay smooth. Rows keep their position in the set for screen readers.
 * Inputs: accessible label, row count, row height, overscan, row renderer, viewport height. */
export function WindowedList({ label, count, rowHeight = 44, overscan = 10, viewport = 560, renderRow, className }: {
  label: string;
  count: number;
  rowHeight?: number;
  overscan?: number;
  viewport?: number;
  renderRow: (index: number) => ReactNode;
  className?: string;
}) {
  const [scroll, setScroll] = useState({ top: 0, height: viewport });
  const { start, end } = windowRange(scroll.top, scroll.height, rowHeight, count, overscan);
  return <div className={`windowed${className ? ` ${className}` : ""}`} role="list" aria-label={label}
    onScroll={(event) => setScroll({ top: event.currentTarget.scrollTop, height: event.currentTarget.clientHeight })}>
    <div className="windowed-inner" style={{ height: count * rowHeight }}>
      {Array.from({ length: end - start }, (_, offset) => {
        const index = start + offset;
        return <div key={index} role="listitem" aria-setsize={count} aria-posinset={index + 1} className="windowed-row"
          style={{ top: index * rowHeight, height: rowHeight }}>{renderRow(index)}</div>;
      })}
    </div>
  </div>;
}
