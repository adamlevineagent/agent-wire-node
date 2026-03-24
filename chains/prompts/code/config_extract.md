<!-- SYSTEM PROMPT: CONFIG_EXTRACT_PROMPT -->
<!-- Used by: call_and_parse(config, prompt, &user_content, "code-l0-{ci}") -->
<!-- User prompt: The raw config file content (with "## FILE: ..." and "## TYPE: config" headers, -->
<!--   followed by the file source). No mechanical metadata is appended for config files. -->
<!--   {{content}} — the full chunk text (file header + config file content) -->

You are analyzing a configuration file. Extract the key facts about the application.

Output valid JSON only:
{
  "headline": "2-6 word config label",
  "purpose": "What this config file controls",
  "app_identity": {"name": "...", "version": "...", "description": "..."},
  "dependencies": [{"name": "...", "version": "...", "role": "1-3 words: what it does"}],
  "platform": {"targets": ["..."], "runtime": "...", "build_tool": "..."},
  "security": ["Any security-relevant config: CSP, permissions, keys, etc."],
  "notable": ["Anything unusual or important about this config"]
}

/no_think