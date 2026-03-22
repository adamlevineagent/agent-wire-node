import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PyramidDashboard } from '../PyramidDashboard';
import { PyramidFirstRun } from '../PyramidFirstRun';

interface SlugInfo {
    slug: string;
    content_type: string;
    source_path: string;
    node_count: number;
    max_depth: number;
    last_built_at: string | null;
    created_at: string;
}

interface PyramidConfigInfo {
    api_key_set: boolean;
    auth_token_set: boolean;
    primary_model: string;
    fallback_model_1: string;
    fallback_model_2: string;
}

export function PyramidsMode() {
    const [showFirstRun, setShowFirstRun] = useState(false);
    const [checking, setChecking] = useState(true);

    useEffect(() => {
        (async () => {
            try {
                const [slugs, config] = await Promise.all([
                    invoke<SlugInfo[]>('pyramid_list_slugs'),
                    invoke<PyramidConfigInfo>('pyramid_get_config'),
                ]);
                // Show first-run if no slugs AND no API key configured
                if (slugs.length === 0 && !config.api_key_set) {
                    setShowFirstRun(true);
                }
            } catch {
                // If commands fail, just show the dashboard
            } finally {
                setChecking(false);
            }
        })();
    }, []);

    if (checking) {
        return (
            <div className="mode-container">
                <div className="pyramid-loading">Loading...</div>
            </div>
        );
    }

    if (showFirstRun) {
        return <PyramidFirstRun onComplete={() => setShowFirstRun(false)} />;
    }

    return (
        <div className="mode-container">
            <PyramidDashboard />
        </div>
    );
}
