# Architecture

CI writes this file from the module graph on each commit to `main`.

```mermaid
flowchart LR
    subgraph adapters
        crate_adapters_ha[ha]
    end
    subgraph app
        crate_app_background[background]
    end
    subgraph domain
        crate_domain_contracts[contracts]
        crate_domain_intent[intent]
        crate_domain_state[state]
    end
    subgraph ports
        crate_ports[ports]
    end
    subgraph root
        crate[crate]
    end
    subgraph testing
        crate_testing_fakes[fakes]
    end
    crate --> crate
    crate_adapters_ha --> crate
    crate_adapters_ha --> crate_adapters_ha
    crate_adapters_ha --> crate_domain_state
    crate_app_background --> crate_domain_state
    crate_app_background --> crate_ports
    crate_domain_contracts --> crate_domain_state
    crate_domain_intent --> crate_domain_contracts
    crate_domain_intent --> crate_domain_intent
    crate_domain_state --> crate_domain_contracts
    crate_domain_state --> crate_domain_state
    crate_ports --> crate_domain_state
```
