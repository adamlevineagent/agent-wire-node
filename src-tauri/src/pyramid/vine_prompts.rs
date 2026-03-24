// pyramid/vine_prompts.rs — LLM prompts specific to vine construction and intelligence passes.
//
// The vine reuses DISTILL_PROMPT and THREAD_NARRATIVE_PROMPT from build.rs for L1→L2 synthesis.
// These prompts are vine-specific: temporal clustering, ERA detection, transitions, entity resolution.

/// Vine L1 clustering prompt — groups conversation bunches into temporal-topical neighborhoods.
/// Unlike THREAD_CLUSTER_PROMPT which expects L1 topic JSON, this receives bunch summaries
/// with temporal metadata and produces bunch-level clusters.
pub const VINE_CLUSTER_PROMPT: &str = r#"You are organizing conversation sessions into temporal-topical clusters for a project timeline.

Each session has:
- A bunch_index (temporal order, 0 = earliest)
- A date range
- A list of topics discussed
- Key entities mentioned

Your job: group these sessions into coherent CLUSTERS based on temporal proximity AND topical similarity. A cluster represents a phase of work — "the authentication design sessions", "the DADBEAR implementation sprint", etc. Use as many clusters as needed to cover all sessions.

Rules:
- Prefer temporal contiguity: nearby sessions cluster together UNLESS topics diverge sharply
- Max 4 sessions per cluster (keeps distillation manageable)
- Singletons are allowed for sessions that don't fit any cluster
- Every session must be assigned to exactly ONE cluster
- Use clear, descriptive cluster names that describe what was being worked on
- Sessions with overlapping topics SHOULD cluster even with small temporal gaps

Output valid JSON only:
{
  "clusters": [
    {
      "name": "Cluster Name — describes the work phase",
      "description": "1-2 sentences: what these sessions collectively accomplished",
      "bunch_indices": [0, 1, 2]
    }
  ]
}

/no_think"#;

/// ERA phase boundary detection — binary classifier for adjacent session pairs.
pub const VINE_PHASE_CHECK_PROMPT: &str = r#"You are analyzing two adjacent conversation sessions from a project to determine if they represent the SAME project phase or DIFFERENT phases.

Session A (earlier) and Session B (later) are described below with their topics and entities.

A phase change means:
- Fundamentally different focus (new feature, new system, new problem domain)
- Same entities but different activity (designing → debugging → deploying)
- Significant vocabulary shift

NOT a phase change:
- Continuing the same work
- Deepening the same topic
- Minor tangent before returning to main work

Output valid JSON only:
{
  "same_phase": true/false,
  "confidence": 0.0-1.0,
  "reason": "1 sentence explaining why"
}

/no_think"#;

/// Transition classification between adjacent ERAs.
pub const VINE_TRANSITION_PROMPT: &str = r#"You are classifying the transition between two project phases (ERAs).

ERA A (earlier) and ERA B (later) are described below.

Classify the transition as exactly ONE of:
- "pivot": Fundamentally different focus. Old topics abandoned, new ones dominate.
- "evolution": Conceptual continuity with vocabulary shift. Same problem, deeper understanding.
- "expansion": Old topics persist, new topics added. Broadening scope.
- "refinement": Same topics, deeper focus. Tightening execution.
- "return": Topics from a previous era resurface after a gap.

Output valid JSON only:
{
  "transition_type": "pivot|evolution|expansion|refinement|return",
  "from_era": "ERA A label",
  "to_era": "ERA B label",
  "reason": "1-2 sentences explaining the transition"
}

/no_think"#;

/// Entity resolution — cluster variant entity names to canonical forms.
pub const VINE_ENTITY_RESOLUTION_PROMPT: &str = r#"You are resolving entity name variants across multiple conversation sessions.

Below is a list of entity name clusters. Each cluster contains names that MIGHT refer to the same thing based on string similarity. Your job: for each cluster, decide if they ARE the same entity, and if so, pick the canonical (best) name.

Rules:
- Only merge entities that genuinely refer to the same thing
- The canonical name should be the most specific, clear, and commonly used form
- If a cluster contains entities that are actually different things, split them
- Abbreviations and full names of the same thing should merge

Output valid JSON only:
{
  "resolved": [
    {
      "canonical": "The best name for this entity",
      "aliases": ["variant1", "variant2", "abbreviation"],
      "description": "What this entity is, in 1 sentence"
    }
  ],
  "rejected": [
    {
      "reason": "Why these were NOT merged",
      "entities": ["entity1", "entity2"]
    }
  ]
}

/no_think"#;
