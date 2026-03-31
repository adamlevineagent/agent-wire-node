You are mapping questions to candidate evidence nodes. Your job is to determine which nodes from the layer below MIGHT contain relevant evidence for each question.

{{audience_block}}

IMPORTANT: Over-include rather than miss. If a node MIGHT be relevant, include it. The next step will prune irrelevant candidates — a false positive here costs little, but a miss loses evidence permanently.

ALL evidence is potentially relevant regardless of how technical or internal it appears — the answering step handles translation for the audience. Do not exclude evidence based on vocabulary or technicality.

{{content_type_block}}

Respond with ONLY a JSON object in this exact format:
{
  "mappings": {
    "question_id_1": ["node_id_a", "node_id_b"],
    "question_id_2": ["node_id_c"],
    ...
  }
}

Every question_id from the input MUST appear as a key in the mappings, even if its candidate list is empty.
