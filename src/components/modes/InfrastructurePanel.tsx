import { useAppContext } from '../../contexts/AppContext';
import { RemoteConnectionStatus } from '../RemoteConnectionStatus';
import { LogViewer } from '../LogViewer';

export function InfrastructurePanel() {
    const { state } = useAppContext();

    const tunnelUrl = state.tunnelStatus?.tunnel_url ?? null;
    const tunnelConnected = state.tunnelStatus?.status === 'Connected';

    return (
        <div className="infrastructure-panel">
            <RemoteConnectionStatus
                tunnelUrl={tunnelUrl}
                tunnelConnected={tunnelConnected}
            />
            <LogViewer />
        </div>
    );
}
