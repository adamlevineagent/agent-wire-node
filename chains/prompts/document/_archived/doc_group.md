<!-- SYSTEM PROMPT: DOC_GROUP_PROMPT -->
<!-- Used by: call_and_parse(llm_config, DOC_GROUP_PROMPT, &user_prompt, "doc-l1-{ci_idx}") -->
<!-- User prompt template: -->
<!--   ## DOCUMENTS IN THIS CLUSTER -->
<!--   {{doc_names}}     — comma-separated list of document names in this cluster -->
<!--   -->
<!--   ## CONTENT -->
<!--   {{child_data}}    — JSON of L0 extraction results for each document in the cluster -->

You are grouping related documents from a creative fiction project. These documents have been clustered because they share characters, locations, or plot threads.

Describe what this cluster covers as a unit. What storylines, character arcs, or worldbuilding threads connect these documents?

For each topic:
- name: A clear name for this narrative thread
- current: What the reader knows at this point
- characters: Characters involved
- plot_status: Where this thread stands (setup / developing / climax / resolved / open)
- connections: How this thread connects to other parts of the story
- headline: a 2-6 word label for this grouped node. Concrete and reader-friendly.

Output valid JSON only:
{
  "headline": "2-6 word story arc label",
  "orientation": "1-2 sentences: what this cluster covers and which documents to read for which threads.",
  "topics": [
    {
      "name": "Thread/Arc Name",
      "current": "Where this thread stands",
      "entities": ["character 1", "location 1", "plot element 1"],
      "plot_status": "setup / developing / climax / resolved / open",
      "connections": ["connects to thread X via character Y"]
    }
  ]
}

/no_think