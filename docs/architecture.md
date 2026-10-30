# Architecture

CI writes this file from the module graph on each commit to `main`.

```mermaid
flowchart LR
    subgraph domain
        crate_domain_contracts[contracts]
    end
    subgraph root
        crate[crate]
    end
```
