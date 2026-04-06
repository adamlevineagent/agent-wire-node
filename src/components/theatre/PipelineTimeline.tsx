import type { LayerProgress } from './types';

interface PipelineTimelineProps {
    currentStep: string | null;
    layers: LayerProgress[];
}

/** Canonical pipeline phases in execution order */
const PIPELINE_PHASES = [
    { key: 'load_state', label: 'Load State' },
    { key: 'source_extract', label: 'Extract' },
    { key: 'l0_webbing', label: 'L0 Web' },
    { key: 'delta_refresh', label: 'Refresh' },
    { key: 'enhance_questions', label: 'Enhance Q' },
    { key: 'question_decomposition', label: 'Decompose' },
    { key: 'schema_alignment', label: 'Schema' },
    { key: 'evidence_loop', label: 'Evidence' },
    { key: 'process_gaps', label: 'Gaps' },
    { key: 'l1_webbing', label: 'L1 Web' },
    { key: 'l2_webbing', label: 'L2 Web' },
];

type ChipState = 'pending' | 'active' | 'complete' | 'skipped';

export function PipelineTimeline({ currentStep, layers }: PipelineTimelineProps) {
    // Derive chip states from currentStep and completed layers
    const completedSteps = new Set(
        layers
            .filter(l => l.status === 'complete')
            .map(l => l.step_name)
    );

    const activeSteps = new Set(
        layers
            .filter(l => l.status === 'active')
            .map(l => l.step_name)
    );

    const chipStates = PIPELINE_PHASES.map(phase => {
        let state: ChipState = 'pending';
        if (completedSteps.has(phase.key)) {
            state = 'complete';
        } else if (activeSteps.has(phase.key) || currentStep === phase.key) {
            state = 'active';
        }
        return { ...phase, state };
    });

    // Mark phases before the current active as complete if they weren't tracked
    let seenActive = false;
    for (let i = chipStates.length - 1; i >= 0; i--) {
        if (chipStates[i].state === 'active') seenActive = true;
        if (seenActive && chipStates[i].state === 'pending') {
            chipStates[i].state = 'complete';
        }
    }

    return (
        <div className="pipeline-timeline">
            {chipStates.map(chip => (
                <div
                    key={chip.key}
                    className={`pipeline-chip pipeline-chip-${chip.state}`}
                >
                    {chip.state === 'complete' && <span className="pipeline-check">&#10003;</span>}
                    {chip.label}
                </div>
            ))}
        </div>
    );
}
