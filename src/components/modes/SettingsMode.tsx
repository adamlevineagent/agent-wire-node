import { Settings } from '../Settings';
import { PyramidSettings } from '../PyramidSettings';

export function SettingsMode() {
    return (
        <div className="mode-container">
            <PyramidSettings />
            <div style={{ marginTop: '2rem', borderTop: '1px solid rgba(255,255,255,0.1)', paddingTop: '2rem' }}>
                <Settings />
            </div>
        </div>
    );
}
