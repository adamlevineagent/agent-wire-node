/**
 * Widget catalog — defines the UI components the planner can include in plan previews.
 * The planner LLM sees this catalog and produces ui_schema entries referencing these types.
 * Sprint 0: defined for reference. Sprint 1: used by the planner.
 */

export const WIDGET_CATALOG = [
    {
        type: 'corpus_selector',
        description: 'Select from available corpora',
        props: { multi: 'boolean', filter: 'string?' },
    },
    {
        type: 'text_input',
        description: 'Free text input field',
        props: { field: 'string', label: 'string', placeholder: 'string?' },
    },
    {
        type: 'cost_preview',
        description: 'Show estimated cost breakdown',
        props: { amount: 'number', breakdown: 'object?' },
    },
    {
        type: 'toggle',
        description: 'Boolean toggle with label',
        props: { field: 'string', label: 'string', default: 'boolean?' },
    },
    {
        type: 'agent_selector',
        description: 'Select from available agents',
        props: { multi: 'boolean', filter: 'string?' },
    },
    {
        type: 'confirmation',
        description: 'Review and confirm action',
        props: { summary: 'string', details: 'string?' },
    },
] as const;

export type WidgetType = typeof WIDGET_CATALOG[number]['type'];
