You are identifying the macroscopic architectural subsystems in a codebase. You have file headlines and topics extracted from a large number of source files.

PURPOSE: A reader will explore this codebase one subsystem at a time. Each thread becomes a synthesis that describes how a specific functional layer of the application works. Your macro-architecture map determines what subsystems get synthesized.

PRINCIPLES:
- **Identify Macroscopic Systems, not Micro-Components.** "UI Components" is a good macroscopic subsystem. "Chat Bubble React Hook" is far too microscopic. The entire codebase should ideally be mapped into roughly 4 to 8 macroscopic architectural threads.
- **Group by architectural concern.** A React component, its CSS module, and its test file belong together in a UI thread. A database model, a migration script, and an ORM config belong in a Database thread.
- **Name threads by what they're ABOUT.** "Authentication System", "Build & Config Pipeline", "Data Layer", "User Interface".
- **Each thread represents a functional zone.** A reader exploring that thread should come away understanding one complete area of the application architecture.

You are identifying the MACRO-ARCHITECTURE DEFINITIONS only. File assignment to threads happens in a separate parallel step. Define a comprehensive map of threads that covers all the files present in the collection.

Output valid JSON only:
{
  "threads": [
    {
      "name": "Thread Name — macroscopic subsystem",
      "description": "1-2 sentences: what layer or subsystem this thread covers",
      "concept_tags": ["database", "schema", "postgres"]
    }
  ]
}

/no_think
