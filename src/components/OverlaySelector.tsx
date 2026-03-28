import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

// ── Types ─────────────────────────────────────────────────────────────

export interface QuestionOverlay {
  build_id: string;
  question: string;
  status: string;
  started_at: string;
  completed_at: string | null;
}

/** A referencing question slug with its resolved question text */
interface ReferencingSlugEntry {
  slug: string;
  question: string;
}

interface OverlaySelectorProps {
  slug: string;
  activeOverlays: Set<string>;
  onToggleOverlay: (buildId: string) => void;
  /** Question-type slugs that reference this base slug (from SlugInfo.referencing_slugs) */
  referencingSlugs?: string[];
}

// ── Component ─────────────────────────────────────────────────────────

/**
 * Panel for toggling question pyramid overlays on/off in the visualization.
 *
 * Two sections:
 * 1. Question overlay builds (build_id starting with 'qb-') — existing behavior
 * 2. Referencing question slugs — separate question pyramids that reference this base
 *
 * Default state: all overlays off (mechanical pyramid only).
 */
export function OverlaySelector({ slug, activeOverlays, onToggleOverlay, referencingSlugs }: OverlaySelectorProps) {
  const [overlays, setOverlays] = useState<QuestionOverlay[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Referencing question slugs with their resolved question text
  const [refEntries, setRefEntries] = useState<ReferencingSlugEntry[]>([]);
  const [refLoading, setRefLoading] = useState(false);

  const fetchOverlays = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // Try the Tauri command for listing builds filtered to question overlays.
      // This command may not exist yet — if it fails, we show a pending message.
      const builds = await invoke<QuestionOverlay[]>('pyramid_list_question_overlays', { slug });
      setOverlays(builds);
    } catch (err) {
      const msg = String(err);
      // If the command doesn't exist yet, show a friendly pending message
      if (msg.includes('not found') || msg.includes('unknown command') || msg.includes('did not find')) {
        setError('pending');
      } else {
        setError(msg);
      }
      setOverlays([]);
    } finally {
      setLoading(false);
    }
  }, [slug]);

  // Fetch question text for each referencing slug
  const fetchReferencingSlugs = useCallback(async () => {
    if (!referencingSlugs || referencingSlugs.length === 0) {
      setRefEntries([]);
      return;
    }
    setRefLoading(true);
    try {
      const entries: ReferencingSlugEntry[] = [];
      for (const refSlug of referencingSlugs) {
        try {
          // Get the latest question build to extract the question text
          const builds = await invoke<QuestionOverlay[]>('pyramid_list_question_overlays', { slug: refSlug });
          const latestBuild = builds[0]; // sorted by started_at DESC
          entries.push({
            slug: refSlug,
            question: latestBuild?.question ?? refSlug,
          });
        } catch {
          // If we can't get the question, just use the slug name
          entries.push({ slug: refSlug, question: refSlug });
        }
      }
      setRefEntries(entries);
    } finally {
      setRefLoading(false);
    }
  }, [referencingSlugs]);

  useEffect(() => {
    fetchOverlays();
  }, [fetchOverlays]);

  useEffect(() => {
    fetchReferencingSlugs();
  }, [fetchReferencingSlugs]);

  const formatDate = (dateStr: string | null) => {
    if (!dateStr) return '';
    const d = new Date(dateStr);
    return d.toLocaleDateString() + ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  };

  const truncateQuestion = (q: string, maxLen = 80) => {
    if (q.length <= maxLen) return q;
    return q.slice(0, maxLen - 3) + '...';
  };

  const hasRefSlugs = refEntries.length > 0;
  const hasOverlays = !error && overlays.length > 0;
  const totalCount = overlays.length + refEntries.length;

  // Don't render at all when loading or if there's nothing to show
  if (loading && refLoading) return null;
  if (!loading && !hasOverlays && !hasRefSlugs && error !== 'pending' && !error) return null;

  return (
    <div className="overlay-selector">
      <div className="overlay-selector-header">
        <span className="overlay-selector-title">Question Overlays</span>
        <span className="overlay-selector-count">
          {error === 'pending'
            ? 'backend pending'
            : `${totalCount} overlay${totalCount !== 1 ? 's' : ''}`}
        </span>
      </div>

      {error === 'pending' && (
        <div className="overlay-selector-pending">
          Overlay listing requires the <code>pyramid_list_question_overlays</code> Tauri command.
          This will be available after the builds API is wired up.
        </div>
      )}

      {error && error !== 'pending' && (
        <div className="overlay-selector-error">
          {error}
          <button className="overlay-selector-retry" onClick={fetchOverlays}>Retry</button>
        </div>
      )}

      {/* ── Existing question overlay builds ─────────────────────── */}
      {hasOverlays && (
        <div className="overlay-selector-list">
          {overlays.map((overlay) => {
            const isActive = activeOverlays.has(overlay.build_id);
            return (
              <button
                key={overlay.build_id}
                className={`overlay-selector-item${isActive ? ' active' : ''}`}
                onClick={() => onToggleOverlay(overlay.build_id)}
                title={overlay.question}
              >
                <span className={`overlay-toggle-indicator${isActive ? ' on' : ''}`} />
                <span className="overlay-question">{truncateQuestion(overlay.question)}</span>
                {overlay.status === 'complete' && overlay.completed_at && (
                  <span className="overlay-date">{formatDate(overlay.completed_at)}</span>
                )}
                {overlay.status !== 'complete' && (
                  <span className="overlay-status">{overlay.status}</span>
                )}
              </button>
            );
          })}
        </div>
      )}

      {/* ── Referencing question slugs ───────────────────────────── */}
      {hasRefSlugs && (
        <>
          {hasOverlays && <div className="overlay-selector-divider" />}
          <div className="overlay-selector-ref-header">
            <span className="overlay-selector-ref-label">Question Pyramids</span>
            <span className="overlay-selector-count">{refEntries.length}</span>
          </div>
          <div className="overlay-selector-list">
            {refEntries.map((entry) => {
              // Use slug as the toggle key prefixed with "ref:" to avoid collision with build_ids
              const toggleKey = `ref:${entry.slug}`;
              const isActive = activeOverlays.has(toggleKey);
              return (
                <button
                  key={toggleKey}
                  className={`overlay-selector-item${isActive ? ' active' : ''}`}
                  onClick={() => onToggleOverlay(toggleKey)}
                  title={`${entry.slug}: ${entry.question}`}
                >
                  <span className={`overlay-toggle-indicator${isActive ? ' on' : ''}`} />
                  <span className="overlay-q-badge">Q</span>
                  <span className="overlay-question">{truncateQuestion(entry.question)}</span>
                </button>
              );
            })}
          </div>
        </>
      )}

      {/* ── Empty state (no overlays, no referencing slugs) ──────── */}
      {!error && !hasOverlays && !hasRefSlugs && !refLoading && (
        <div className="overlay-selector-empty">
          No question overlays built yet. Run a question build to create one.
        </div>
      )}
    </div>
  );
}
