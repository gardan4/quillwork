import { useEffect, useImperativeHandle, useLayoutEffect, useRef, type CSSProperties, type Ref } from "react";
import { render, type QuillworkView } from "./viewer";
import type { Asset, Patch, PatchResult, ReviewAction, ViewerOptions, ViewerSelection, ViewModel } from "./types";

export type { QuillworkView } from "./viewer";
export type * from "./types";

export interface QuillworkViewerProps {
  /** The document. A new object re-renders everything (a full reload). */
  viewModel: ViewModel;
  /**
   * Patches to apply on top of `viewModel`, in order. Append to the array as patches arrive;
   * each patch object is applied once, and patches at or below the shown version are skipped.
   */
  patches?: readonly Patch[];
  onReview?: (changeId: string, action: ReviewAction) => void;
  onSelect?: (selection: ViewerSelection | null) => void;
  assetUrl?: (assetId: string, asset: Asset | undefined) => string;
  /** A patch did not fit (wrong base version or unknown anchor): fetch the full view model. */
  onNeedsReload?: (result: Extract<PatchResult, { ok: false }>, patch: Patch) => void;
  /** Called after each applied patch. */
  onPatched?: (result: Extract<PatchResult, { ok: true }>) => void;
  theme?: ViewerOptions["theme"];
  showHidden?: boolean;
  rail?: boolean;
  flashMs?: number;
  className?: string;
  style?: CSSProperties;
  /** The underlying view: highlight(), scrollTo(), getSelection(), ... */
  ref?: Ref<QuillworkView | null>;
}

/** React 19 wrapper around `render()`. */
export function QuillworkViewer(props: QuillworkViewerProps) {
  const { viewModel, patches, className, style, ref } = props;
  const box = useRef<HTMLDivElement>(null);
  const view = useRef<QuillworkView | null>(null);
  const applied = useRef(new WeakSet<Patch>());
  const shownVm = useRef<ViewModel | null>(null);
  const latest = useRef(props);
  latest.current = props;

  // Callbacks go through `latest`, so changing them never re-renders the document.
  const options = (): ViewerOptions => ({
    onReview: props.onReview ? (id, a) => latest.current.onReview?.(id, a) : undefined,
    onSelect: (s) => latest.current.onSelect?.(s),
    assetUrl: props.assetUrl,
    theme: props.theme,
    showHidden: props.showHidden,
    rail: props.rail,
    flashMs: props.flashMs,
  });

  useLayoutEffect(() => {
    const v = render(box.current!, viewModel, options());
    view.current = v;
    shownVm.current = viewModel;
    applied.current = new WeakSet();
    return () => {
      v.destroy();
      view.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useImperativeHandle(ref, () => view.current!, []);

  useEffect(() => {
    view.current?.setOptions({
      theme: props.theme,
      showHidden: props.showHidden,
      rail: props.rail,
      flashMs: props.flashMs,
      assetUrl: props.assetUrl,
      onReview: props.onReview ? (id, a) => latest.current.onReview?.(id, a) : undefined,
    });
  }, [props.theme, props.showHidden, props.rail, props.flashMs, props.assetUrl, !!props.onReview]);

  useLayoutEffect(() => {
    const v = view.current;
    if (!v) return;
    if (shownVm.current !== viewModel) {
      shownVm.current = viewModel;
      applied.current = new WeakSet();
      v.setViewModel(viewModel);
    }
    let failed = false;
    for (const p of patches ?? []) {
      if (applied.current.has(p)) continue;
      applied.current.add(p);
      // After a failed patch the rest cannot apply either; the host reloads the view model.
      if (failed || p.version <= v.version) continue;
      const r = v.applyPatch(p);
      if (r.ok) latest.current.onPatched?.(r);
      else {
        failed = true;
        latest.current.onNeedsReload?.(r, p);
      }
    }
  }, [viewModel, patches]);

  return <div ref={box} className={className} style={style} />;
}

export default QuillworkViewer;
