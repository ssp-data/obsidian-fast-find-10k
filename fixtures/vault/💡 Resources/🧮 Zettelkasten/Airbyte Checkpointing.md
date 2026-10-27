---
tags: [ingestion]
---
# Airbyte Checkpointing

Airbyte persists state after each sync so an interrupted job resumes from the last
checkpoint instead of starting over. Incremental streams store a cursor per stream.

## State messages
- Sources emit state messages between records.
- The destination commits them once the batch is durable.
