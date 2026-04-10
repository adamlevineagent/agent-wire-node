// Shared widget prop contract.
//
// Every widget in `src/components/yaml-renderer/widgets/` takes the
// same shape: the current `value`, a `onChange` callback, an
// optional `disabled` flag, the `annotation` describing the field,
// and the pre-resolved `optionSources` map (for widgets that draw
// from dynamic option sources).
//
// Keeping this uniform lets `YamlConfigRenderer` dispatch through a
// single switch on `widget` without per-widget special-casing.

import type {
  FieldAnnotation,
  OptionValue,
} from "../../../types/yamlRenderer";

export interface WidgetProps {
  /** The current YAML value for this field. May be undefined if unset. */
  value: unknown;
  /**
   * Callback the widget invokes when the user edits the value. The
   * renderer folds the path prefix in so widgets only need to pass
   * the new scalar.
   */
  onChange: (next: unknown) => void;
  /** True when the renderer is in readOnly mode or the field is flagged. */
  disabled?: boolean;
  /** The field annotation describing how to render this widget. */
  annotation: FieldAnnotation;
  /** Pre-resolved dynamic options keyed by source name. */
  optionSources: Record<string, OptionValue[]>;
  /** Optional per-field cost estimate. */
  costEstimate?: number;
}
