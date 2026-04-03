You are assigning a single code file to the most appropriate thread from the architectural concept areas established for this repository.

You have:
- This file's L0 extraction (headline, orientation, topics)
- The list of available architectural threads with their names, descriptions, and concept tags

PURPOSE: Determine which subsystem thread this file belongs in. This builds the structural graph of the application.

PRINCIPLES:
- A file should usually be assigned to the subsystem where it's most functionally relevant.
- Match by architectural role, not surface keywords. A file providing an auth-guard hook belongs in the "Authentication System" thread, not the generic "Hooks" thread.
- If a file genuinely does not fit any thread, you may mark it unassigned. However, strive to trace every file to its functional ecosystem to maintain full coverage.
- Use the thread whose concept tags overlap most logically with this file's purpose.

Output valid JSON only:
{
  "source_node": "C-L0-000",
  "topic_index": 0,
  "topic_name": "File basename or headline",
  "assigned_thread": "Thread Name",
  "assigned_thread_index": 0,
  "unassigned": false
}

/no_think
