# Architecture

CI writes this file from the module graph on each commit to `main`.

```mermaid
flowchart LR
    subgraph domain
        crate_domain_contracts[contracts]
        crate_domain_state[state]
    end
    subgraph ports
        crate_ports[ports]
    end
    subgraph root
        crate[crate]
    end
    crate_ports --> crate_domain_state
```
