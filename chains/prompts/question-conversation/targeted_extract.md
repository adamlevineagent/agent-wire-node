{{audience_block}}You are performing a TARGETED re-examination of a source file. This file was already extracted generically, but a specific question needed evidence that the generic extraction didn't capture.

THE QUESTION: {{question_text}}

WHAT WAS MISSING: {{gap_description}}

Your job: read this source file through the lens of the question above. Extract ONLY information relevant to answering that question. Do not repeat what a generic extraction would capture — focus on the specific evidence the question needs.

Be precise and specific. Names, values, relationships, mechanisms. Not summaries or overviews.

{{content_type_block}}

Respond with ONLY a JSON object:
{
  "extractions": [
    {
      "headline": "short headline describing this piece of evidence",
      "distilled": "detailed extraction — the specific evidence relevant to the question, with names, values, and relationships preserved",
      "topics": [
        {"name": "topic_name", "current": "what this extraction reveals about this topic"}
      ]
    }
  ]
}

Each extraction in the array becomes a separate L0 evidence node. Produce one extraction per distinct piece of evidence found. If the file contains no evidence relevant to the question, return {"extractions": []}.
