import { useState, useEffect, useMemo, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface FaqNode {
    id: string;
    slug: string;
    question: string;
    answer: string;
    related_node_ids: string[];
    annotation_ids: number[];
    hit_count: number;
    match_triggers: string[];
    created_at: string;
    updated_at: string;
}

interface FaqCategory {
    id: string;
    slug: string;
    name: string;
    distilled_summary: string;
    faq_ids: string[];
    created_at: string;
    updated_at: string;
}

interface FaqCategoryEntry {
    category: FaqCategory;
    faq_count: number;
    children: FaqNode[] | null;
}

interface FaqDirectory {
    slug: string;
    mode: string; // "flat" | "hierarchical"
    total_faqs: number;
    categories: FaqCategoryEntry[];
    uncategorized: FaqNode[];
}

interface FAQDirectoryProps {
    slug: string;
    onBack: () => void;
}

export function FAQDirectory({ slug, onBack }: FAQDirectoryProps) {
    const [directory, setDirectory] = useState<FaqDirectory | null>(null);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [searchQuery, setSearchQuery] = useState('');
    const [expandedFaqId, setExpandedFaqId] = useState<string | null>(null);
    const [expandedCategoryId, setExpandedCategoryId] = useState<string | null>(null);
    const [categoryChildren, setCategoryChildren] = useState<Record<string, FaqNode[]>>({});
    const [loadingCategory, setLoadingCategory] = useState<string | null>(null);

    const fetchDirectory = useCallback(async () => {
        setLoading(true);
        try {
            const data = await invoke<FaqDirectory>('pyramid_faq_directory', { slug });
            setDirectory(data);
            setError(null);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    }, [slug]);

    useEffect(() => {
        fetchDirectory();
    }, [fetchDirectory]);

    const handleDrillCategory = useCallback(async (categoryId: string) => {
        if (expandedCategoryId === categoryId) {
            setExpandedCategoryId(null);
            return;
        }

        if (categoryChildren[categoryId]) {
            setExpandedCategoryId(categoryId);
            return;
        }

        setLoadingCategory(categoryId);
        try {
            const data = await invoke<FaqCategoryEntry>('pyramid_faq_category_drill', {
                slug,
                categoryId,
            });
            if (data.children) {
                setCategoryChildren(prev => ({ ...prev, [categoryId]: data.children! }));
            }
            setExpandedCategoryId(categoryId);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoadingCategory(null);
        }
    }, [slug, expandedCategoryId, categoryChildren]);

    const toggleFaq = useCallback((faqId: string) => {
        setExpandedFaqId(prev => prev === faqId ? null : faqId);
    }, []);

    // Client-side filter across questions + match_triggers
    const filteredFaqs = useMemo(() => {
        if (!directory) return [];
        const q = searchQuery.toLowerCase().trim();
        if (!q) return directory.uncategorized;
        return directory.uncategorized.filter(faq => {
            if (faq.question.toLowerCase().includes(q)) return true;
            if (faq.answer.toLowerCase().includes(q)) return true;
            return faq.match_triggers.some(t => t.toLowerCase().includes(q));
        });
    }, [directory, searchQuery]);

    const filteredCategories = useMemo(() => {
        if (!directory || directory.mode !== 'hierarchical') return [];
        const q = searchQuery.toLowerCase().trim();
        if (!q) return directory.categories;
        return directory.categories.filter(entry => {
            if (entry.category.name.toLowerCase().includes(q)) return true;
            if (entry.category.distilled_summary.toLowerCase().includes(q)) return true;
            return false;
        });
    }, [directory, searchQuery]);

    if (loading) {
        return (
            <div className="faq-directory">
                <div className="faq-directory-header">
                    <button className="btn btn-ghost" onClick={onBack}>&larr; Back</button>
                    <h2>FAQ Directory</h2>
                </div>
                <div className="pyramid-loading">Loading FAQ directory...</div>
            </div>
        );
    }

    if (error) {
        return (
            <div className="faq-directory">
                <div className="faq-directory-header">
                    <button className="btn btn-ghost" onClick={onBack}>&larr; Back</button>
                    <h2>FAQ Directory</h2>
                </div>
                <div className="pyramid-error">
                    {error}
                    <button className="workspace-error-dismiss" onClick={() => setError(null)}>
                        Dismiss
                    </button>
                </div>
            </div>
        );
    }

    if (!directory) return null;

    return (
        <div className="faq-directory">
            <div className="faq-directory-header">
                <button className="btn btn-ghost" onClick={onBack}>&larr; Back</button>
                <div>
                    <h2>FAQ Directory &mdash; {slug}</h2>
                    <span className="faq-directory-meta">
                        {directory.total_faqs} FAQs &middot; {directory.mode} mode
                    </span>
                </div>
            </div>

            <div className="faq-search">
                <input
                    type="text"
                    placeholder="Search FAQs..."
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                    className="faq-search-input"
                />
            </div>

            {/* Hierarchical mode: category cards */}
            {directory.mode === 'hierarchical' && filteredCategories.length > 0 && (
                <div className="faq-categories">
                    {filteredCategories.map(entry => (
                        <div key={entry.category.id} className="faq-category-card">
                            <div
                                className="faq-category-card-header"
                                onClick={() => handleDrillCategory(entry.category.id)}
                            >
                                <div className="faq-category-card-title">
                                    <span className="faq-category-expand">
                                        {expandedCategoryId === entry.category.id ? '\u25BC' : '\u25B6'}
                                    </span>
                                    <h3>{entry.category.name}</h3>
                                    <span className="faq-category-count">{entry.faq_count} FAQs</span>
                                </div>
                                <p className="faq-category-summary">{entry.category.distilled_summary}</p>
                            </div>

                            {expandedCategoryId === entry.category.id && (
                                <div className="faq-category-children">
                                    {loadingCategory === entry.category.id && (
                                        <div className="pyramid-loading">Loading...</div>
                                    )}
                                    {categoryChildren[entry.category.id]?.map(faq => (
                                        <FaqEntry
                                            key={faq.id}
                                            faq={faq}
                                            expanded={expandedFaqId === faq.id}
                                            onToggle={() => toggleFaq(faq.id)}
                                        />
                                    ))}
                                </div>
                            )}
                        </div>
                    ))}
                </div>
            )}

            {/* Flat mode or uncategorized FAQs */}
            {filteredFaqs.length > 0 && (
                <div className="faq-flat-list">
                    {directory.mode === 'hierarchical' && filteredFaqs.length > 0 && (
                        <h3 className="faq-section-title">Uncategorized</h3>
                    )}
                    {filteredFaqs.map(faq => (
                        <FaqEntry
                            key={faq.id}
                            faq={faq}
                            expanded={expandedFaqId === faq.id}
                            onToggle={() => toggleFaq(faq.id)}
                        />
                    ))}
                </div>
            )}

            {filteredFaqs.length === 0 && filteredCategories.length === 0 && searchQuery && (
                <div className="faq-empty">No FAQs match "{searchQuery}"</div>
            )}

            {directory.total_faqs === 0 && (
                <div className="faq-empty">
                    No FAQs yet. Annotate pyramid nodes with questions to build the FAQ.
                </div>
            )}
        </div>
    );
}

// ── Individual FAQ entry component ──────────────────────────────────

interface FaqEntryProps {
    faq: FaqNode;
    expanded: boolean;
    onToggle: () => void;
}

function FaqEntry({ faq, expanded, onToggle }: FaqEntryProps) {
    return (
        <div className={`faq-entry ${expanded ? 'faq-entry-expanded' : ''}`}>
            <div className="faq-entry-header" onClick={onToggle}>
                <span className="faq-entry-expand">
                    {expanded ? '\u25BC' : '\u25B6'}
                </span>
                <span className="faq-entry-question">{faq.question}</span>
                <span className="faq-entry-hits" title="Hit count">{faq.hit_count} hits</span>
            </div>

            {expanded && (
                <div className="faq-entry-body">
                    <div className="faq-entry-answer">{faq.answer}</div>

                    {faq.match_triggers.length > 0 && (
                        <div className="faq-entry-triggers">
                            <span className="faq-entry-label">Triggers:</span>
                            {faq.match_triggers.map((t, i) => (
                                <span key={i} className="faq-trigger-tag">{t}</span>
                            ))}
                        </div>
                    )}

                    {faq.related_node_ids.length > 0 && (
                        <div className="faq-entry-sources">
                            <span className="faq-entry-label">Sources:</span>
                            {faq.related_node_ids.map((nid, i) => (
                                <span key={i} className="faq-source-tag">{nid}</span>
                            ))}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
}
