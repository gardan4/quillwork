/**
 * The viewer's text model: how a rendered paragraph maps to the character offsets that
 * getSelection() and highlight() use.
 *
 * Offsets count the paragraph's accepted-mode text, the same text `find` and `expect` match:
 * - text and field results count as shown;
 * - a tab is "\t", a line, page or column break is "\n";
 * - deleted and moved-away text, list labels, note marks, images and the viewer's own
 *   markers count nothing.
 *
 * Elements carry this in two attributes: `data-qw-x` (the subtree counts nothing) and
 * `data-qw-t` (the element counts as this string, its children are ignored).
 */

export const ATTR_SKIP = "data-qw-x";
export const ATTR_TEXT = "data-qw-t";

type Unit = { node: Node; text: string };

function units(root: Element): Unit[] {
  const out: Unit[] = [];
  const walk = (n: Node) => {
    for (let c = n.firstChild; c; c = c.nextSibling) {
      if (c.nodeType === 3) out.push({ node: c, text: (c as Text).data });
      else if (c.nodeType === 1) {
        const e = c as Element;
        if (e.hasAttribute(ATTR_SKIP)) continue;
        const t = e.getAttribute(ATTR_TEXT);
        if (t !== null) out.push({ node: e, text: t });
        else walk(e);
      }
    }
  };
  walk(root);
  return out;
}

/** Accepted-mode text of a rendered paragraph. */
export function blockText(p: Element): string {
  let s = "";
  for (const u of units(p)) s += u.text;
  return s;
}

/** Offset of the DOM point (container, offset) inside paragraph `p`. */
export function offsetOf(p: Element, container: Node, offset: number): number {
  const doc = p.ownerDocument;
  const point = doc.createRange();
  point.setStart(container, offset);
  let n = 0;
  for (const u of units(p)) {
    if (u.node === container) return n + Math.min(offset, u.text.length);
    if (u.node.nodeType === 1 && u.node.contains(container)) return n;
    // (u.node, 0) after the point: the point lies before this unit.
    if (point.comparePoint(u.node, 0) > 0) return n;
    n += u.text.length;
  }
  return n;
}

/** DOM point for character `offset` in paragraph `p`. */
export function pointAt(p: Element, offset: number): { node: Node; offset: number } {
  let n = 0;
  let last: Unit | undefined;
  for (const u of units(p)) {
    const len = u.text.length;
    if (offset <= n + len && len > 0) {
      if (u.node.nodeType === 3) return { node: u.node, offset: offset - n };
      const parent = u.node.parentNode as Node;
      const idx = Array.prototype.indexOf.call(parent.childNodes, u.node);
      return { node: parent, offset: offset > n ? idx + 1 : idx };
    }
    n += len;
    last = u;
  }
  if (last && last.node.nodeType === 3) return { node: last.node, offset: (last.node as Text).data.length };
  return { node: p, offset: p.childNodes.length };
}
