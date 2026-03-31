/**
 * Wire action metadata for the planner and published chains.
 *
 * Sprint 2: PLANNER_ACTION_ID is set after the planner action is published
 * to the Wire via POST /api/v1/contribute. Until published, it remains a
 * placeholder and the planner runs locally without a Wire-side action.
 *
 * Supersession strategy: when the planner prompt changes materially,
 * publish a new action contribution that supersedes the previous one,
 * and update PLANNER_ACTION_ID here.
 */

export const PLANNER_ACTION_ID = '<placeholder-uuid>';

export const PLANNER_ACTION_META = {
    title: 'Wire Node Intent Planner v1',
    description: 'Takes user intent + context, returns a structured plan with named vocabulary commands.',
    type: 'action' as const,
    subtype: 'planner',
};

/**
 * All locally known tool metadata.
 * Sprint 2: the planner + any chains published from this node.
 * Published chains fetched from Wire are merged at runtime in ToolsMode.
 */
export const LOCAL_TOOLS = [
    {
        id: PLANNER_ACTION_ID,
        ...PLANNER_ACTION_META,
        published: false,
        usageCount: 0,
    },
];

/**
 * Build a chain contribution body from an executed plan.
 * Strips execution results — just the reusable recipe.
 * Body is serialized to a JSON string for the contribute endpoint.
 */
export function buildChainDefinition(
    steps: Array<{
        command?: string;
        args?: Record<string, unknown>;
        navigate?: unknown;
        description: string;
        on_error?: string;
    }>,
    uiSchema: Array<{ type: string; field?: string }>,
): string {
    // Extract required commands from steps
    const requiredCommands = steps
        .filter(s => s.command)
        .map(s => s.command!)
        .filter((v, i, a) => a.indexOf(v) === i); // dedupe

    // Extract input schema from widget fields
    const inputFields: Record<string, string> = {};
    for (const widget of uiSchema) {
        if (widget.field && widget.type !== 'confirmation' && widget.type !== 'cost_preview') {
            inputFields[widget.field] = widget.type === 'toggle' ? 'boolean' : 'string';
        }
    }

    return JSON.stringify({
        format_version: 1,
        steps: steps.map(s => ({
            command: s.command,
            args: s.args,
            navigate: s.navigate,
            description: s.description,
            on_error: s.on_error,
        })),
        input_schema: inputFields,
        required_commands: requiredCommands,
    });
}
