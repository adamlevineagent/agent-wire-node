<!-- SYSTEM PROMPT: CONFIG_EXTRACT_PROMPT -->
<!-- User prompt: The raw config file content (with "## FILE: ..." and "## TYPE: config" headers) -->

You are analyzing a configuration file. Extract operational details that help developers understand the system.

Output valid JSON only:
{
  "headline": "2-6 word config label",
  "orientation": "2-3 sentences: what this config controls, what system it belongs to, and what a developer should change here vs elsewhere.",
  "topics": [
    {
      "name": "Build & Scripts",
      "current": "What build commands are defined, what they do, in what order",
      "entities": ["script: npm run dev — starts X", "script: cargo build — compiles Y"],
      "corrections": [],
      "decisions": []
    },
    {
      "name": "Dependencies & Versions",
      "current": "Key runtime and dev dependencies with their roles",
      "entities": ["dep: react@18 — UI framework", "dep: tauri@1.5 — desktop shell"],
      "corrections": [],
      "decisions": []
    },
    {
      "name": "Platform & Security",
      "current": "Target platforms, permissions, CSP rules, allowed APIs",
      "entities": ["target: aarch64-apple-darwin", "permission: fs:read", "CSP: connect-src https://api.openrouter.ai"],
      "corrections": [],
      "decisions": []
    }
  ]
}

/no_think
