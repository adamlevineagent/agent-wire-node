# Experiment Log — Planner Command Names

## Lab Initialized
Branch: `research/planner-command-names` @ 02cdb17
Objective: Make intent planner use exact vocabulary command names instead of inventing
Focus: `chains/prompts/planner/planner-system.md` + `chains/vocabulary/*.md`
Model: inception/mercury-2 @ temperature 0.3

## Experiment 0 — Baseline
Branch: research/planner-command-names / Type: real / Parent: -
Hypothesis: Establish current behavior with no changes
Changes: None
Result: 5/5 steps valid (100%) — BUT intent#1 ("archive agents with zero contributions") produced navigate:fleet instead of actual API calls. The model is dodging complex multi-step plans by falling back to navigation.
Duration: 7.7s total
Status: keep (baseline)
Insight: The model uses correct names when it does produce commands/API calls (intents 2,3,4 all correct). The failure mode from the handoff — invented names like `list_agents` — may surface when the model is forced to produce multi-step API plans instead of navigating away. Need to test whether the navigate cop-out is the prompt teaching it to prefer simple plans, or whether forcing multi-step plans triggers invented names.

## THINK — before Experiment 1

**Convergence signals**: Baseline is unexpectedly good on command names. The real problem may have shifted from "invents names" to "avoids producing plans that would require names it's unsure about."

**Untested assumptions**: 
- Does the model invent names when forced to produce multi-step API plans?
- Is the navigate fallback because the prompt says "If the operation you need is not in the vocabulary, use a navigate step"?
- The 3 examples might be teaching it: example 1 = commands, example 2 = single API call, example 3 = navigate. Intent#1 (complex filtering + archiving) matches none well, so it defaults to navigate.

**Next hypothesis**: Adding a guideline that says "Prefer API call plans over navigate steps when the vocabulary contains the needed endpoints" will push the model toward producing actual multi-step plans. This should surface the invented-names problem if it still exists.
