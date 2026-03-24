import { useEffect, useState, type RefObject } from 'react';

/**
 * Hook that manages DPI scaling and responsive sizing for one or more canvases
 * within a container element.
 *
 * - Sets canvas width/height attributes to match container * devicePixelRatio
 * - Uses ResizeObserver for responsive sizing
 * - Cleans up on unmount
 *
 * Returns the current container dimensions in CSS pixels.
 */
export function useCanvasSetup(
  canvasRefs: RefObject<HTMLCanvasElement | null>[],
  containerRef: RefObject<HTMLDivElement | null>,
): { width: number; height: number } {
  const [size, setSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    let frameId = 0;
    let observer: ResizeObserver | null = null;
    let disposed = false;

    const syncSize = () => {
      const container = containerRef.current;
      if (!container) return;

      const rect = container.getBoundingClientRect();
      const w = Math.floor(rect.width);
      const h = Math.floor(rect.height);
      const dpr = window.devicePixelRatio || 1;

      setSize({ width: w, height: h });

      for (const ref of canvasRefs) {
        const canvas = ref.current;
        if (!canvas) continue;
        canvas.width = w * dpr;
        canvas.height = h * dpr;
        canvas.style.width = `${w}px`;
        canvas.style.height = `${h}px`;
        const ctx = canvas.getContext('2d');
        if (ctx) {
          ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        }
      }
    };

    const attachObserver = () => {
      if (disposed) return;

      const container = containerRef.current;
      const hasCanvas = canvasRefs.some((ref) => ref.current);
      if (!container || !hasCanvas) {
        frameId = window.requestAnimationFrame(attachObserver);
        return;
      }

      syncSize();
      observer = new ResizeObserver(syncSize);
      observer.observe(container);
    };

    attachObserver();

    return () => {
      disposed = true;
      if (frameId) {
        window.cancelAnimationFrame(frameId);
      }
      observer?.disconnect();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerRef, ...canvasRefs]);

  return size;
}
