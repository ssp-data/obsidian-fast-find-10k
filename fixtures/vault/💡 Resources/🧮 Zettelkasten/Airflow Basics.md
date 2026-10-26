---
tags: [orchestration]
---
# Airflow Basics

DAGs, operators, sensors and the scheduler. Keep tasks idempotent and push
heavy work out of the scheduler into the workers.

## Gotchas
- Top-level code in DAG files runs on every parse.
- XComs are for small values only.
