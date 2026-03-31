#!/usr/bin/env python3
"""
Planner prompt tester — assembles the prompt exactly like Rust does,
calls OpenRouter, and scores whether commands are valid.
"""

import json
import os
import re
import sys
import time
import urllib.request

# --- Config ---
PROJ = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PROMPT_PATH = os.path.join(PROJ, "chains/prompts/planner/planner-system.md")
VOCAB_DIR = os.path.join(PROJ, "chains/vocabulary")
CONFIG_PATH = os.path.expanduser("~/Library/Application Support/wire-node/pyramid_config.json")

with open(CONFIG_PATH) as f:
    cfg = json.load(f)

API_KEY = cfg["openrouter_api_key"]
MODEL = cfg.get("primary_model", "inception/mercury-2")

# --- Valid commands (from IntentBar.tsx) ---
ALLOWED_COMMANDS = {
    "pyramid_build", "pyramid_create_slug", "pyramid_build_cancel",
    "pyramid_list_slugs", "sync_content", "get_sync_status", "save_compose_draft",
}

ALLOWED_API_PATTERNS = [
    "POST /api/v1/wire/agents/archive",
    "PATCH /api/v1/operator/agents/*/status",
    "POST /api/v1/wire/tasks",
    "PUT /api/v1/wire/tasks/*",
    "POST /api/v1/wire/rate",
    "POST /api/v1/contribute",
]

# All command names from vocabulary (broader than executor allowlist)
VOCAB_COMMANDS = set()  # populated from vocabulary files

# --- Test intents ---
TEST_INTENTS = [
    "Please archive all my agents with zero contributions",
    "Build a pyramid from my agent-wire-node code",
    "Search the Wire for battery chemistry",
    "Create a task: review auth security",
]

# --- Mock context (what the frontend gathers) ---
MOCK_CONTEXT = {
    "pyramids": [
        {"slug": "agent-wire-node", "node_count": 142, "content_type": "codebase"},
        {"slug": "core-docs", "node_count": 88, "content_type": "documents"},
    ],
    "corpora": [
        {"slug": "agent-wire-node-code", "path": "/Users/me/agent-wire-node", "doc_count": 340},
    ],
    "agents": [
        {"id": "a1", "name": "ember", "status": "active"},
        {"id": "a2", "name": "scout-3", "status": "active"},
        {"id": "a3", "name": "archivist", "status": "offline"},
    ],
    "fleet": {"online_count": 2, "task_count": 3},
    "balance": 4200,
}

# --- Widget catalog (simplified) ---
WIDGET_CATALOG = """Available widget types: corpus_selector, text_input, select, agent_selector, toggle, cost_preview, confirmation"""


def assemble_prompt():
    """Assemble the full system prompt exactly as Rust does."""
    with open(PROMPT_PATH) as f:
        template = f.read()

    vocab_parts = []
    for fname in sorted(os.listdir(VOCAB_DIR)):
        if fname.endswith(".md"):
            with open(os.path.join(VOCAB_DIR, fname)) as f:
                content = f.read()
                vocab_parts.append(content)
                # Extract command names for vocabulary validation
                for m in re.finditer(r"###\s+(\S+)", content):
                    VOCAB_COMMANDS.add(m.group(1))
                for m in re.finditer(r'"command":\s*"([^"]+)"', content):
                    VOCAB_COMMANDS.add(m.group(1))

    full_vocab = "\n\n---\n\n".join(vocab_parts)
    context_json = json.dumps(MOCK_CONTEXT, indent=2)

    result = template.replace("{{VOCABULARY}}", full_vocab)
    result = result.replace("{{WIDGET_CATALOG}}", WIDGET_CATALOG)
    result = result.replace("{{CONTEXT}}", context_json)
    return result


def call_openrouter(system_prompt, user_message):
    """Call OpenRouter API."""
    payload = json.dumps({
        "model": MODEL,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_message},
        ],
        "temperature": 0.3,
        "max_tokens": 4096,
        "response_format": {"type": "json_object"},
    }).encode()

    req = urllib.request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=payload,
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "Content-Type": "application/json",
            "HTTP-Referer": "http://localhost:1420",
        },
    )

    with urllib.request.urlopen(req, timeout=60) as resp:
        data = json.loads(resp.read())

    content = data["choices"][0]["message"]["content"]
    # Try to parse JSON from the response
    try:
        return json.loads(content)
    except json.JSONDecodeError:
        # Try to extract JSON from markdown code block
        m = re.search(r"```(?:json)?\s*(\{.*?\})\s*```", content, re.DOTALL)
        if m:
            return json.loads(m.group(1))
        raise ValueError(f"Could not parse JSON from response: {content[:500]}")


def is_api_path_allowed(method, path):
    """Check if API path matches executor allowlist."""
    key = f"{method} {path}"
    for pattern in ALLOWED_API_PATTERNS:
        regex = "^" + pattern.replace("*", "[^/]+") + "$"
        if re.match(regex, key):
            return True
    return False


def score_step(step):
    """Score a single step. Returns (valid, detail)."""
    if "navigate" in step and step["navigate"]:
        return True, f"navigate:{step['navigate'].get('mode', '?')}"

    if "command" in step and step["command"]:
        cmd = step["command"]
        if cmd in ALLOWED_COMMANDS:
            return True, f"command:{cmd} ✓ (executor-allowed)"
        elif cmd in VOCAB_COMMANDS:
            return False, f"command:{cmd} ✗ (in vocabulary but not in executor allowlist)"
        else:
            return False, f"command:{cmd} ✗ INVENTED (not in vocabulary)"

    if "api_call" in step and step["api_call"]:
        method = step["api_call"].get("method", "?")
        path = step["api_call"].get("path", "?")
        if is_api_path_allowed(method, path):
            return True, f"api:{method} {path} ✓ (executor-allowed)"
        else:
            return False, f"api:{method} {path} ✗ (not in executor allowlist)"

    return False, "no command/api_call/navigate found"


def run_test(system_prompt, intent_idx=None):
    """Run the test suite — all intents or a specific one."""
    intents = [TEST_INTENTS[intent_idx]] if intent_idx is not None else TEST_INTENTS
    total_steps = 0
    valid_steps = 0
    results = []

    for intent in intents:
        print(f"\n{'='*60}")
        print(f"INTENT: {intent}")
        print(f"{'='*60}")

        try:
            t0 = time.time()
            plan = call_openrouter(system_prompt, intent)
            elapsed = time.time() - t0
            print(f"Response time: {elapsed:.1f}s")

            steps = plan.get("steps", [])
            print(f"Steps: {len(steps)}")

            intent_valid = 0
            intent_total = len(steps)

            for step in steps:
                valid, detail = score_step(step)
                status = "✓" if valid else "✗"
                desc = step.get("description", "no description")
                print(f"  {status} {desc}")
                print(f"    → {detail}")
                if valid:
                    valid_steps += 1
                    intent_valid += 1
                total_steps += 1

            rate = (intent_valid / intent_total * 100) if intent_total > 0 else 0
            results.append({
                "intent": intent,
                "valid": intent_valid,
                "total": intent_total,
                "rate": rate,
                "plan": plan,
            })
        except Exception as e:
            print(f"ERROR: {e}")
            results.append({"intent": intent, "valid": 0, "total": 0, "rate": 0, "error": str(e)})

    overall_rate = (valid_steps / total_steps * 100) if total_steps > 0 else 0
    print(f"\n{'='*60}")
    print(f"OVERALL: {valid_steps}/{total_steps} valid steps ({overall_rate:.0f}%)")
    print(f"{'='*60}")

    return {
        "valid_steps": valid_steps,
        "total_steps": total_steps,
        "overall_rate": overall_rate,
        "per_intent": results,
    }


if __name__ == "__main__":
    prompt = assemble_prompt()
    print(f"Prompt assembled: {len(prompt):,} chars (~{len(prompt)//4:,} tokens)")
    print(f"Vocabulary commands found: {len(VOCAB_COMMANDS)}")
    print(f"Model: {MODEL}")

    idx = int(sys.argv[1]) if len(sys.argv) > 1 else None
    result = run_test(prompt, idx)

    # Save raw results
    out_path = os.path.join(PROJ, ".lab/workspace/last_result.json")
    with open(out_path, "w") as f:
        json.dump(result, f, indent=2)
    print(f"\nRaw results saved to {out_path}")
