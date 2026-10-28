# Architecture

CI writes this file from the module graph on each commit to `main`.

```mermaid
flowchart LR
    subgraph root
        crate[crate]
    end
```
