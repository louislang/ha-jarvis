# Phase 1: Configuration

This phase loads `jarvis.toml` and the secrets. Each later sub-phase adds only the configuration section that it uses.

---

### 1.1 Configuration File

**Goal:** Add `Config`, which parses `jarvis.toml` with defaults and rejects unknown fields. Jarvis now has one typed source for all settings, and a typo in the file fails at startup.

- **Implements:** G10. **Depends on:** 0.15.
- **Files:** create `jarvis.toml`. Modify `src/main.rs`, `Cargo.toml`.

**Model.**

- `Config::parse(text) -> Result<Config>` is pure. `Config::load(path) -> Result<Config>` reads the file and calls `parse`.
- This sub-phase has the fields `listen`, `device` (`cuda` or `cpu`) and `[ha]` (`url`, `token_env`). Later sub-phases add their sections (`01-architecture.md` section 9).
- A field with a default in section 9 uses that default when it is absent. A field without a default is required.
- All structs use `deny_unknown_fields`.

**Acceptance criteria.**

- AC-1.1-1: When the text is a valid configuration, `parse` returns a `Config` with each value in the text.
- AC-1.1-2: When the file does not exist, `load` returns an error that contains the path.
- AC-1.1-3: When a field has the wrong type, `parse` returns an error that contains the field name.
- AC-1.1-4: When a required field is absent, `parse` returns an error that contains the field name.
- AC-1.1-5: When the text has an unknown field, `parse` returns an error that contains the field name.
- AC-1.1-6: When `device` is not `cuda` or `cpu`, `parse` returns an error that contains `device`.

**Proofs.**

- P-1.1-1 (AC-1.1-1). Requires: the text is valid TOML with all required fields of the correct types. Ensures: each field of the result equals the value in the text. Argument: `Config` derives `Deserialize` with one field for each key. Deserialization copies each value into its field without change.
- P-1.1-2. Evidence: test.
- P-1.1-3 to P-1.1-5. Argument: the derived deserializer returns `Err` for a wrong type, a missing field without a default, and, with `deny_unknown_fields`, an unknown field. Each error names the key.
- P-1.1-6 (AC-1.1-6). Argument: `device` is an `enum` with the two variants `cuda` and `cpu`. Any other string has no variant, so deserialization fails.

---

### 1.2 Secrets from the Environment

**Goal:** Add `Secrets`, which reads each secret from the environment variable that the configuration names. Secrets stay out of files, and a missing secret fails at startup with its variable name.

- **Implements:** G10. **Depends on:** 1.1.
- **Files:** modify `src/main.rs`.

**Model.** `Secrets::resolve(config, lookup) -> Result<Secrets>`. `lookup: Fn(&str) -> Option<String>` is the environment. `main` passes `std::env::var`, and tests pass a map. A secret is valid when the variable is set and not empty. This sub-phase resolves `ha.token_env`. Later sub-phases add their secrets.

**Acceptance criteria.**

- AC-1.2-1: When `HA_TOKEN` is `abc`, `resolve` returns `ha_token = "abc"`.
- AC-1.2-2: When `HA_TOKEN` is not set, `resolve` returns an error that contains `HA_TOKEN`.
- AC-1.2-3: When `HA_TOKEN` is an empty string, `resolve` returns an error that contains `HA_TOKEN`.

**Proofs.**

- P-1.2-1 to P-1.2-3. Requires: `lookup` is a total function. Ensures: `Ok` if and only if `lookup(name)` is `Some(v)` with `v` not empty, and then the secret is `v`. Argument: `resolve` matches on `lookup(name)`. `None` and `Some("")` give `Err(name)`. Every other `Some(v)` gives `Ok(v)`. `resolve` has no other branch.
