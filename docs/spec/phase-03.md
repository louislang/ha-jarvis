# Phase 3: System One Model

This phase adds the intent catalog, the training set contract, the Python training script, the fixture model and local prediction. The fixture model makes all later phases testable without RunPod or a GPU.

---

### 3.1 Normalize

**Goal:** Add `normalize` to the domain, so that every text that Jarvis compares goes through one normalization.

- **Implements:** G3. **Depends on:** 2.9.
- **Files:** create `src/domain/intent.rs`. Modify `src/lib.rs`.

**Model.**

- `normalize(text) -> String`: lowercase the ASCII letters `A` to `Z`, replace each character that is not a letter, a digit, a space or an apostrophe with a space (a `.` between two digits stays), collapse runs of spaces, trim. A letter that is not ASCII stays unchanged, because Verus cannot prove that Unicode lowercase is idempotent. Jarvis uses `normalize` for comparisons (choices, feedback keys, correlation, the held-out split, seed equality). The model also gets the normalized transcript, because speech-to-text adds capitals and punctuation that the training examples do not have. Slot spans and slots are byte offsets in the normalized text.

**Acceptance criteria.**

- AC-3.1-1: When `normalize("Turn ON the  Lights!")` runs, it returns `"turn on the lights"`.
- AC-3.1-2: When `normalize` runs on its own output, the result does not change.
- AC-3.1-7: When `normalize("Set it to 72.5, twenty-two")` runs, it returns `"set it to 72.5 twenty two"`.

**Proofs.**

- P-3.1-2 (AC-3.1-2). Claim: `normalize(normalize(t)) = normalize(t)`. Argument: after one pass, the text has only lowercase ASCII letters, other letters, digits, apostrophes, dots between digits and single inner spaces, with no outer spaces. Each step maps such a text to itself, so the second pass changes nothing.
- P-3.1-1, P-3.1-7. Evidence: test.

---

### 3.1b Example Markup

**Goal:** Add `parse_example` to the domain, so that seed examples can mark slot values with `[value](slot)`.

- **Implements:** G3. **Depends on:** 3.1.
- **Files:** modify `src/domain/intent.rs`.

**Model.** `parse_example(markup) -> Result<Example>`: `Example { text, spans: Vec<Span> }`. `Span { slot, start, end }` are byte offsets into `text`. All Rust code uses byte offsets. Only the training set file uses character offsets (sub-phase 3.3). The text is the markup with each `[value](slot)` replaced by `value`.

**Acceptance criteria.**

- AC-3.1b-1: When `parse_example("turn on the [kitchen](area) lights")` runs, it returns the text "turn on the kitchen lights" and the span `area` from 12 to 19.
- AC-3.1b-2: When an example has two slots, both spans are correct and in text order.
- AC-3.1b-3: When an example has `[` without a closing `](slot)`, `parse_example` returns an error that contains the byte offset of the `[` in the markup.
- AC-3.1b-4: When a slot name is empty, `parse_example` returns an error.

**Proofs.**

- P-3.1b-1, P-3.1b-2 (AC-3.1b-1, AC-3.1b-2). Ensures: each span is inside the bytes of `text`, and the spans are ordered and do not overlap. Argument: the parser copies characters to `text` from left to right. When it reads a slot, it records `start = len(text)`, copies `value`, and records `end = len(text)`. Offsets only grow, so spans are ordered and disjoint. The tests are the evidence for `text[start..end] == value`.
- P-3.1b-3, P-3.1b-4 (AC-3.1b-3, AC-3.1b-4). Ensures: a `[` without a closing `](slot)`, or an empty slot name, gives `Err`. Argument: the parser is a small state machine (`Text`, `Value`, `Close`, `Slot`). At the end of input, any state other than `Text` is an error. An empty slot name is rejected when `Slot` ends.

---

### 3.2 Intent Catalog

**Goal:** Add `intents.toml`, the catalog types and `validate_catalog`, which checks the example counts, the example markup and the intent names. The catalog is now the single source of the System One intents.

- **Implements:** G3. **Depends on:** 3.1b.
- **Files:** create `intents.toml`, `src/domain/catalog.rs`. Modify `src/lib.rs`.

**Model.**

- `Catalog { min_examples, unknown_examples, intents: Vec<Intent> }`. `Intent` has the fields of `01-architecture.md` section 9. `examples` are the markup strings. The catalog types derive `Deserialize`.
- The domain does not read TOML, because `toml` is not a domain crate. The caller parses `intents.toml` with `toml::from_str` (sub-phase 5.4). The tests do the same.
- `validate_catalog(catalog) -> Result<(), CatalogError>` checks:
  1. Each intent has `min_examples` examples or more.
  2. Each example parses (`parse_example`).
  3. Names are unique, and no name is `unknown`.
- The first `intents.toml` has `lights_on`, `fan_on`, `climate_down`, `tv_show`, `temperature_query` and `weather` from `01-architecture.md` section 9. Sub-phases 11.6, 13.11 and 15.7 add `tv_on`, `start_training` and `calendar_add`.

**Acceptance criteria.**

- AC-3.2-1: When `intents.toml` is parsed, the catalog has every intent in the file, and `validate_catalog` returns `Ok`.
- AC-3.2-2: When an intent has fewer than `min_examples` examples, `validate_catalog` returns an error that contains the intent name.
- AC-3.2-3: When an example has a markup error, `validate_catalog` returns an error that contains the intent name.
- AC-3.2-5: When two intents have the same name, `validate_catalog` returns an error that contains the name.
- AC-3.2-6: When an intent is named `unknown`, `validate_catalog` returns an error.

**Proofs.**

- P-3.2-2, P-3.2-3, P-3.2-5, P-3.2-6. Ensures: `validate_catalog` returns `Ok` if and only if checks 1 to 3 pass. Argument: the function runs the checks in sequence and returns the first `Err`. Each check is a `for` loop over a finite list with one comparison.
- P-3.2-1. Evidence: test.

---

### 3.2b Template Slots and Normalized Examples

**Goal:** Add two checks to `validate_catalog`: every template variable comes from a slot, and every example is normalized.

- **Implements:** G3. **Depends on:** 3.2.
- **Files:** modify `src/domain/catalog.rs`, `Cargo.toml`.

**Model.**

- This sub-phase adds `minijinja` (`01-architecture.md` section 7). Sub-phase 4.2 uses the same crate.
- `validate_catalog` adds these checks:
  4. Each variable of a template (the `entity` field and the `data` values) that is not a built-in (`area`, `entity`, `person`, `response`) and not a `habit_defaults` key is tagged in one or more examples of the intent. The check gets the variables from `minijinja` `undeclared_variables` and collects them into a `BTreeSet`. A template that does not parse gives an error.
  5. The text of each example (after the markup) equals its `normalize` result.

**Acceptance criteria.**

- AC-3.2b-1: When a template uses a slot that no example of the intent tags, `validate_catalog` returns an error that contains the slot name. A `habit_defaults` key and a built-in variable give no error.
- AC-3.2b-2: When the text of an example is not normalized (for example it has a capital letter or a comma), `validate_catalog` returns an error that contains the example.
- AC-3.2b-3: When a template does not parse, `validate_catalog` returns an error that contains the intent name.

**Proofs.**

- P-3.2b-1 to P-3.2b-3. Evidence: test. Verus keeps one direction of P-3.2: `Ok` implies that checks 1 to 3 pass. The other direction no longer holds, because check 4 or 5 can fail when checks 1 to 3 pass. The `ensures` of `parse_example` (P-3.1b-1) does not state the text or the slot names of an example, so a Verus clause cannot connect checks 4 and 5 to the markup.

---

### 3.2c Labels and Tags

**Goal:** Add `labels` and `tags`. The catalog is now the single source of the System One labels.

- **Implements:** G3. **Depends on:** 3.2b.
- **Files:** modify `src/domain/catalog.rs`.

**Model.**

- `tags(catalog) -> Vec<String>`: `O`, then `B-<slot>` and `I-<slot>` for each slot name in sorted order.
- `labels(catalog) -> Vec<String>`: `unknown`, then the intent names in file order.

**Acceptance criteria.**

- AC-3.2c-1: When the slots are `show` and `area`, `tags` returns `O, B-area, I-area, B-show, I-show`.
- AC-3.2c-2: When `labels` runs, the first label is `unknown` and the others are the intent names in file order.

**Proofs.**

- P-3.2c-1 (AC-3.2c-1). Ensures: `tags` is determined by the set of slot names. Argument: the slot names go into a `BTreeSet`, which iterates in sorted order. The output is `O` followed by two entries for each element.
- P-3.2c-2 (AC-3.2c-2). Ensures: `labels(c) = [unknown] ++ names(c.intents)`, in file order. Argument: `labels` prepends `unknown` to a map over the intent list, which keeps the order.

---

### 3.3 Seed Training Set

**Goal:** Add the training set types and `seed_set`, which turns the seed examples into labeled examples with a model context. This fixes the JSON contract between Jarvis and the training script.

- **Implements:** G3, G16. **Depends on:** 3.2c.
- **Files:** create `src/domain/training.rs`. Modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.**

- `TrainingSet { base_model, epochs, batch_size, labels, tags, examples }` (JSON in `01-architecture.md` section 9).
- `Example { context, text, label: Intent(name) | Not(name), spans: Option<Vec<Span>> }`.
- `context_text(area, habit_lines) -> String`: `"area: <area or none> | habits: <lines joined by '; ' or none>"`.
- `sample_context(areas, rng) -> String`: `context_text` of a random area and no habit lines. Sub-phase 13.1 adds habit lines.
- `seed_set(catalog, areas, rng, settings) -> TrainingSet`: one `Intent` example for each seed example and one `unknown` example for each unknown seed. Each example gets `sample_context`.
- The JSON writer converts each span from byte offsets to character offsets, and the reader converts them back (`01-architecture.md` section 9).
- `held_out(text, holdout) -> bool = xxh3(normalize(text)) mod 10000 < holdout * 10000`. This sub-phase adds `train.holdout`.

**Acceptance criteria.**

- AC-3.3-1: When `context_text("kitchen", [])` runs, it returns `"area: kitchen | habits: none"`.
- AC-3.3-2: When `context_text(None, ["a", "b"])` runs, it returns `"area: none | habits: a; b"`.
- AC-3.3-3: When `sample_context` gets the same seed, it returns the same context.
- AC-3.3-4: When `seed_set` runs, the number of examples equals the number of seed examples plus the number of unknown examples.
- AC-3.3-5: When `seed_set` runs, the labels equal `labels(catalog)` and the tags equal `tags(catalog)`.
- AC-3.3-6: When a training set is written to JSON and read again, it is equal.
- AC-3.3-7: When the example is `"set the [café](area) lights"`, the JSON span is 8 to 12 (characters), and the Rust span is 8 to 13 (bytes).
- AC-3.3-8: When `held_out` runs two times for the same text, it returns the same value.
- AC-3.3-9: When `held_out` runs for 1,000 different texts with `holdout = 0.1`, between 70 and 130 are held out.

**Proofs.**

- P-3.3-1, P-3.3-2. Ensures: the result is `area: <area or none> | habits: <lines joined by "; " or none>`. Argument: `context_text` is a `format!` with two branches for empty values.
- P-3.3-3 (AC-3.3-3). Ensures: the same areas and the same seed give the same context. Argument: `sample_context` reads only `rng` and `areas`. A seeded RNG gives the same sequence.
- P-3.3-4 (AC-3.3-4). Ensures: the number of examples is the number of seed examples plus the number of unknown seeds. Argument: `seed_set` maps each seed example to one example and each unknown seed to one example, then concatenates. Map keeps length.
- P-3.3-5. Ensures: the labels equal `labels(catalog)` and the tags equal `tags(catalog)`. Argument: `seed_set` calls `labels` and `tags` (P-3.2c-1, P-3.2c-2).
- P-3.3-7 (AC-3.3-7). Ensures: a byte span `[b, e)` becomes the character span `[chars(text[..b]), chars(text[..e]))`, and the reader gives the byte span back. Argument: the writer counts the characters before each byte offset, and the reader counts the bytes of that many characters. `é` is 2 bytes and 1 character, so the end 13 becomes 12.
- P-3.3-6. Evidence: test (`serde_json`).
- P-3.3-8. Ensures: `held_out(t, h)` depends only on `normalize(t)` and `h`. Argument: `xxh3` with a fixed seed is a pure function, and `normalize` is pure.
- P-3.3-9. Evidence: test (statistical property of the hash on this input set).

---

### 3.4 Dataset Command

**Goal:** Add `jarvis dataset <config> <out> [--house <snapshot.json>]`, which writes the training set from the current data. The fixture model and debugging now have one command that produces the contract file.

- **Implements:** G3. **Depends on:** 3.3.
- **Files:** modify `src/lib.rs`, `jarvis.toml`, `Cargo.toml`.

**Model.** `clap` parses the command line of `jarvis`. The command reads the configuration file `<config>` (`[train]`: `base_model`, `epochs`, `batch_size`) and the catalog file that its key `intents` names. This sub-phase adds `intents`. With `--house`, the areas come from that snapshot. Without it, the area list is empty. It writes the result of `seed_set` as JSON, without the held-out examples. Thus the fixture model never trains on a held-out seed, and the first comparison in sub-phase 13.9 is fair. The RNG has the fixed seed 1, so two runs give the same file.

**Acceptance criteria.**

- AC-3.4-1: When `jarvis dataset jarvis.toml out.json --house src/testing/house.json` runs, `out.json` is a valid training set.
- AC-3.4-2: When it runs with `--house`, each context names an area of the reference house.
- AC-3.4-3: When it runs without `--house`, each context is `"area: none | habits: none"`.
- AC-3.4-4: When the command runs two times with the same inputs, the two files are equal.
- AC-3.4-5: When the command runs, the file has no example for which `held_out` is true.

**Proofs.**

- P-3.4-1, P-3.4-4. Evidence: test.
- P-3.4-5. Ensures: no example in the file has `held_out(text)`. Argument: the writer filters with `not held_out(text)` (P-3.3-8).
- P-3.4-2, P-3.4-3. Ensures: each context names an area of the given list, or `none` when the list is empty. Argument: contexts come from `sample_context`, which chooses only from the given area list, and an empty list gives `none`.

---

### 3.5 Python Project and Gates

**Goal:** Add the Python project in `train/` with its pinned packages, its tool configuration and the command-line entry point. Add the `python` check, so the Python rules are enforced from the first line of Python.

- **Implements:** G10, G17. **Depends on:** 3.4.
- **Files:** create `train/pyproject.toml`, `train/train.py`, `train/test_train.py`. Modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`, `.github/pull_request_template.md`.

**Model.**

- `train/pyproject.toml`: the runtime and `dev` packages of `01-architecture.md` section 7, and the `ruff`, `mypy`, `pytest`, `coverage`, `vulture` and `interrogate` settings of `02-rules.md` section 3. The pins have wheels for Intel macOS: `torch==2.2.2` (the last release with them) and `numpy<2`. torch 2.2.2 has no kernels for GPUs newer than compute capability 9.0, so RunPod uses older GPUs (Phase 13 field log). `mypy` has `ignore_missing_imports` only for the modules without type information (`onnxruntime`, `runpod`).
- `train.py` has `parse_args(argv) -> Args` with `--data`, `--out`, `--device` (`cpu` or `cuda`) and the optional `--max-vram-gb` (sub-phase 3.8).
- `just python` runs, in order: `ruff format --check`, `ruff check`, `mypy --strict`, `vulture --min-confidence 60 --ignore-names forward` (PyTorch calls `forward`), `interrogate --fail-under 100` and `pytest --cov --cov-fail-under=80.01`, all on `train/`.
- `just setup` also makes `.venv` and installs `./train[dev]`. `check` runs `python` after `quint`. The CI job `python` runs `just python`.
- The `complexity` and `duplication` recipes now run on `src xtask train`.

**Acceptance criteria.**

- AC-3.5-1: When `parse_args(["--data", "a.json", "--out", "m", "--device", "cpu"])` runs, it returns the three values.
- AC-3.5-2: When `--device` is `gpu`, `parse_args` raises an error.
- AC-3.5-3: When you run `just python`, it exits with code 0.
- AC-3.5-4: When a fixture Python function has no type annotations, `ruff check` fails with `ANN`. When it returns the wrong type, `mypy` fails.
- AC-3.5-5: When a fixture Python function has no docstring, `interrogate` fails.
- AC-3.5-6: When a fixture Python function is not used, `vulture` fails and names it.
- AC-3.5-7: When a fixture Python function has 51 code lines, cyclomatic complexity 11, Halstead volume over 1000 or Halstead difficulty over 30, `xtask complexity` fails and names it.
- AC-3.5-8: When the `justfile` is dumped, `complexity` and `duplication` run on `train`.
- AC-3.5-9: When `parse_args` gets `--max-vram-gb 1.0`, it returns `1.0`. Without the option, the value is `None`.

**Proofs.**

- P-3.5-1, P-3.5-2. Ensures: `parse_args` returns the three values when `--device` is `cpu` or `cuda`, and raises for any other value. Argument: `--device` uses `choices=["cpu", "cuda"]`, so the parser rejects any other value.
- P-3.5-9. Ensures: `max_vram_gb` is the given number, or `None`. Argument: the option has the type `float` and the default `None`.
- P-3.5-3. Evidence: test.
- P-3.5-4 to P-3.5-7. Evidence: test (gate tests in `xtask/src/gates.rs` run each tool on a fixture file in a temporary folder).
- P-3.5-8. Evidence: test.

---

### 3.6 Training Set Loader and Tag Alignment

**Goal:** Add the training set loader and the alignment of character spans to token tags. The script can now turn the contract file into model inputs, with slot labels only on transcript tokens.

- **Implements:** G3. **Depends on:** 3.5.
- **Files:** modify `train/train.py`, `train/test_train.py`.

**Model.**

- `load_set(path) -> TrainingSet` reads and validates the JSON. Each example has exactly one of `intent` and `not`, and each label is in `labels`.
- `align(spans, offsets, segment_ids, tags) -> list[int]`: for each token, `-100` when the token is in segment A or is special. Else `B-<slot>` for the first token that starts inside a span, `I-<slot>` for the next tokens inside it, and `O` for the rest. `spans = None` gives `-100` for all tokens.

**Acceptance criteria.**

- AC-3.6-1: When the file is a valid training set, `load_set` returns it.
- AC-3.6-2: When an example has both `intent` and `not`, `load_set` raises an error with the example index.
- AC-3.6-3: When a label is not in `labels`, `load_set` raises an error with the label.
- AC-3.6-4: When a span covers "living room", its two tokens get `B-area` and `I-area`.
- AC-3.6-5: When a token is in the context segment, its label is `-100`.
- AC-3.6-6: When `spans` is `None`, all labels are `-100`.

**Proofs.**

- P-3.6-1. Evidence: test.
- P-3.6-2, P-3.6-3. Ensures: `load_set` raises for the first example that has both `intent` and `not`, or a label that is not in `labels`. Argument: the validator checks each example with the two conditions and raises on the first failure.
- P-3.6-4 to P-3.6-6 (AC-3.6-4 to AC-3.6-6). Requires: token offsets are ordered and spans do not overlap (P-3.1b-1). Ensures: each segment B token inside a span gets exactly one `B-` or `I-` label of that span, the first such token gets `B-`, and all other segment B tokens get `O`. Argument: `align` is one pass over tokens with a "current span" pointer that only moves forward. A token whose start is in `[start, end)` of the current span gets `B-` if it is the first one, else `I-`. Segment A, special tokens and `spans = None` take the `-100` branch first.

---

### 3.7 Joint Model and Loss

**Goal:** Add the joint model with an intent head and a slot head on the base encoder, and its loss. `not` examples push down the probability of their label, and examples without spans add no slot loss.

- **Implements:** G3, G15. **Depends on:** 3.6.
- **Files:** modify `train/train.py`, `train/test_train.py`.

**Model.**

- `JointModel(base)`: the encoder, a linear intent head on the first token, a linear slot head on each token. `forward` returns `intent_logits` and `slot_logits`.
- `loss(batch) = intent_loss + slot_loss`.
  - Intent: cross entropy for `intent` examples. `-log(1 - p(not))` for `not` examples.
  - Slot: cross entropy over tokens with label not `-100`.

**Acceptance criteria.**

- AC-3.7-1: When the model runs on a batch of 2 with 5 labels and 7 tags, the shapes are `[2, 5]` and `[2, seq, 7]`.
- AC-3.7-2: When an example has `not = k`, the loss decreases when `p(k)` decreases with all other values fixed.
- AC-3.7-3: When all slot labels are `-100`, the slot loss is 0.
- AC-3.7-4: When an example has `intent = k`, the loss decreases when `p(k)` increases.

**Proofs.**

- P-3.7-2 (AC-3.7-2). Claim: `f(p) = -log(1 - p)` strictly increases on `[0, 1)`. Argument: `f'(p) = 1 / (1 - p) > 0`. So a lower `p(k)` gives a lower loss.
- P-3.7-3 (AC-3.7-3). Ensures: the slot loss is 0 when every slot label is `-100`. Argument: the slot loss is the mean over tokens with a label not `-100`. With no such token, the implementation returns 0 for the empty set.
- P-3.7-4 (AC-3.7-4). Claim: `g(p) = -log(p)` strictly decreases on `(0, 1]`. Argument: `g'(p) = -1 / p < 0`.
- P-3.7-1. Evidence: test.

---

### 3.8 Training Loop and ONNX Export

**Goal:** Add the training loop and the export of `model.onnx`, `tokenizer.json` and `labels.json`. `train.py --data --out --device` now produces a model folder that Jarvis can load.

- **Implements:** G3, G16. **Depends on:** 3.7.
- **Files:** modify `train/train.py`, `train/test_train.py`.

**Model.** `train(set, device, max_vram_gb) -> JointModel` runs `epochs` passes with `batch_size` and AdamW. On `cuda` with `max_vram_gb`, it first calls `torch.cuda.set_per_process_memory_fraction` with `max_vram_gb` divided by the GPU memory, so a run that needs more fails with "CUDA out of memory" (AC-13.10-5 measures it). `export(model, tokenizer, set, out)` writes `model.onnx` (inputs `input_ids`, `attention_mask`, `token_type_ids`; outputs `intent_logits`, `slot_logits`), `tokenizer.json` and `labels.json` (`labels` and `tags` in set order). `main` does `load_set`, `train`, `export`.

**Acceptance criteria.**

- AC-3.8-1: When `train.py` trains on a 10-example set on the CPU with a tiny BERT that the test builds from a `BertConfig` with random weights and a WordPiece tokenizer trained on the set, the output folder has the three files. The test does not download a model.
- AC-3.8-2: When ONNX Runtime and PyTorch run the same input, the outputs are equal within 0.0001.
- AC-3.8-3: When `labels.json` is read, its `labels` and `tags` equal those of the training set, in order.
- AC-3.8-4: When the set has 2 epochs, the training loop runs 2 passes.

**Proofs.**

- P-3.8-3 (AC-3.8-3). Ensures: `labels.json` has the labels and the tags of the set, in order. Argument: `export` writes `set.labels` and `set.tags` without change.
- P-3.8-4 (AC-3.8-4). Ensures: the loop runs `set.epochs` passes. Argument: the loop is `for epoch in range(set.epochs)`.
- P-3.8-1, P-3.8-2. Evidence: test.

---

### 3.9 Fixture Model

**Goal:** Add `just fixture-model`, which trains the System One model on the CPU from the seed examples. CI caches the result, so every later test can use a real model without a GPU.

- **Implements:** G3, G10. **Depends on:** 3.8.
- **Files:** modify `justfile`, `.github/workflows/ci.yml`, `xtask/src/gates.rs`.

**Model.** `fixture-model` first runs `jarvis dataset jarvis.toml target/fixture/train.json --house src/testing/house.json`. `fixture-stale <stamp>` exits with code 0 when `<stamp>` differs from the SHA-256 of `target/fixture/train.json` and the Git-tracked files in `train/`, or when a model file is missing. Thus a change that does not change the training set or the script does not train again. Untracked caches in `train/` do not change the hash. `fixture-model` runs `fixture-stale models/system-one/fixture/inputs.sha256`. If stale, it runs `train.py --device cpu` into `models/system-one/fixture/`, then writes the new stamp. Training uses fixed seeds and deterministic algorithms. `check` runs `fixture-model` before `cov`. The `rust-test` job sets up Python 3.12, installs `./train` and runs `jarvis dataset`. Then it restores `models/system-one/fixture/` from the CI cache with the hash as the key, and runs `just fixture-model` before `just cov`. `fixture-model` is a setup recipe: it can download `train.base_model`, and tests read only its output. CPU training gives different weights on different machines. Thus a criterion that uses the fixture model checks only examples that the model trained on.

**Acceptance criteria.**

- AC-3.9-1: When `just fixture-model` has run, `models/system-one/fixture/` contains the three model files and `inputs.sha256`.
- AC-3.9-2: When `just fixture-model` runs a second time without changes, `model.onnx` keeps the same hash and modification time.
- AC-3.9-3: When `just fixture-stale <stamp>` gets a stamp that differs from the hash of the inputs, it exits with code 0 (stale). With the matching stamp, it exits with code 1.
- AC-3.9-4: When the stamp matches but `model.onnx` is missing, `fixture-stale` exits with code 0. When an untracked file is added to `train/`, the hash does not change.
- AC-3.9-5: When `ci.yml` is parsed, the job `rust-test` installs `./train`, restores the cache with the hash of the training set as the key, and runs `just fixture-model` before `just cov`.
- AC-3.9-6: When only `src/lib.rs` changes and the training set file stays the same, `fixture-stale` exits with code 1.

**Proofs.**

- P-3.9-1. Evidence: test.
- P-3.9-2, P-3.9-3, P-3.9-6. Argument: the recipe trains only when `fixture-stale` exits with code 0, and `fixture-stale` compares the stored hash with the hash of the training set and the script. Evidence: test.
- P-3.9-4, P-3.9-5. Evidence: test.

---

### 3.10 Training Image Release

**Goal:** Add `train/Dockerfile` and the release job that pushes the training image to GitHub Container Registry. RunPod can now run the same training code as the GPU host.

- **Implements:** G16, G17. **Depends on:** 3.9.
- **Files:** create `train/Dockerfile`. Modify `.github/workflows/release.yml`, `xtask/src/gates.rs`.

**Model.** The image starts from a CUDA 12 PyTorch base, installs `./train` (runtime packages only) and runs `python train.py`. Sub-phase 13.8 changes the command to `python train.py --serve`. The release job `train-image` builds it and pushes `ghcr.io/<owner>/jarvis-train:<tag>`.

**Acceptance criteria.**

- AC-3.10-1: When `release.yml` is parsed, the `train-image` job pushes the image, and a later step runs `docker manifest inspect ghcr.io/<owner>/jarvis-train:<tag>`.
- AC-3.10-2: When `release.yml` is parsed, the `train-image` job has a step that runs `pip list` in the image and fails when a `dev` package is present.

**Proofs.**

- P-3.10-1, P-3.10-2. Evidence: test (structure tests of `release.yml` in `xtask/src/gates.rs`).

**Field log.** Push a tag. Record that the `train-image` job passes its verification steps.

---

### 3.11 Model Output Functions

**Goal:** Add the pure functions that turn model outputs into a prediction: softmax, BIO decoding and ranking. The ONNX adapter stays thin, and the logic is testable without a model.

- **Implements:** G3. **Depends on:** 3.10.
- **Files:** create `src/domain/model.rs`. Modify `src/lib.rs`.

**Model.**

- `softmax(logits) -> Vec<f32>` (subtracts the maximum first).
- `decode_bio(text, offsets, tag_ids, tags) -> Vec<Slot>`: `B-x` starts a span of slot `x`. `I-x` continues a span of `x`, or starts one when the previous tag is not `B-x` or `I-x`. `O` ends a span. The value is `text[start of first token .. end of last token]`.
- `ranked(labels, probabilities) -> Vec<(String, f32)>`: sorted by probability, highest first. Equal values keep label order.
- `Prediction { ranked, slots }`.

**Acceptance criteria.**

- AC-3.11-1: When `softmax` runs on one or more finite logits, every value is in `[0, 1]` and the sum is within 0.0001 of 1. The ONNX adapter returns an error for an empty or non-finite output before it calls `softmax`.
- AC-3.11-2: When the logits are `[1000, 0]`, `softmax` returns finite values.
- AC-3.11-3: When the tags are `B-area I-area O` over "living room lights", `decode_bio` returns `area = living room`.
- AC-3.11-4: When `I-area` follows `O`, `decode_bio` starts a new span.
- AC-3.11-5: When `ranked` runs, each probability is greater than or equal to the next one.
- AC-3.11-6: When two labels have the same probability, `ranked` keeps their label order.

**Proofs.**

- P-3.11-1 (AC-3.11-1). Claim (real numbers): `softmax(x)_i = e^(x_i - m) / Σ_j e^(x_j - m)` is in `(0, 1]` and sums to 1. Argument: each term is positive, and the denominator is the sum of the numerators. Floating-point rounding gives the 0.0001 margin.
- P-3.11-2 (AC-3.11-2). Ensures: for finite logits, every output value is finite. Argument: after subtracting `m = max(x)`, every exponent is `<= 0`, so every `e^(x_i - m)` is in `(0, 1]`, and the largest is exactly 1. There is no overflow, and the denominator is at least 1.
- P-3.11-3, P-3.11-4. Ensures: each slot is a maximal run of tokens that starts with `B-x`, or with `I-x` after a tag of another slot or `O`, and continues with `I-x`. Argument: `decode_bio` is one pass with a state "open span of slot `x` or none". Each tag maps to exactly one transition, as listed in the model. `I-x` with state none, or with another slot, takes the "start" transition.
- P-3.11-5, P-3.11-6 (AC-3.11-5, AC-3.11-6). Ensures: the probabilities are non-increasing, and equal probabilities keep the label order. Argument: `ranked` uses a stable sort with a descending comparison. A sort gives a non-increasing order, and a stable sort keeps the input order of equal keys.

---

### 3.12 Classifier Port and ONNX Adapter

**Goal:** Add the `Classifier` and `ModelLoader` ports and their ONNX Runtime adapter with the tokenizer. Jarvis can now load a model folder and predict on the CPU or the GPU.

- **Implements:** G3, G18. **Depends on:** 3.11.
- **Files:** create `src/adapters/onnx.rs`. Modify `src/ports.rs`, `src/lib.rs`, `src/testing/fakes.rs`, `Cargo.toml`.

**Model.**

- `Classifier::predict(context, text) -> Result<Prediction>`. `ModelLoader::load(dir, device) -> Result<Arc<dyn Classifier>>`.
- `OnnxClassifier` tokenizes `(context, text)` as a pair, runs the session, then calls `softmax`, `ranked` and `decode_bio` (sub-phase 3.11).
- `device = cuda` registers the CUDA execution provider. `device = cpu` registers only the CPU provider. Until sub-phase 6.2, `ort` uses the ONNX Runtime binaries that its build downloads.
- `FakeClassifier` returns scripted predictions and records each `(context, text)`. `FakeModelLoader` returns scripted classifiers.

**Acceptance criteria.**

- AC-3.12-1: When the fixture model gets each intent seed example that is not held out, the top label is the intent of the example.
- AC-3.12-2: When the fixture model gets each seed example with an `area` span that is not held out, the slots contain that `area` value.
- AC-3.12-3: When the fixture model gets each seed example with a `show` span that is not held out, the slots contain that `show` value.
- AC-3.12-4: When the fixture model gets each `unknown` seed example that is not held out, the top label is `unknown`.
- AC-3.12-5: When the folder has no `labels.json`, `load` returns an error that names the file.
- AC-3.12-6: When `labels.json` has a different number of labels than `intent_logits`, `load` returns an error with both numbers. When it has a different number of tags than `slot_logits`, `load` returns an error with both numbers.
- AC-3.12-7: When `device = cpu`, the session uses only the CPU execution provider.
- AC-3.12-8 [GPU host]: With `device = cpu`, `predict` with 32 tokens has a p99 of less than 10 ms.
- AC-3.12-9 [GPU host]: With `device = cuda`, the session uses the CUDA execution provider, and `predict` with 32 tokens has a p99 of less than 5 ms.
- AC-3.12-10: When `labels.json` has a duplicate label or tag, `load` returns an error that names it.
- AC-3.12-11: When the input has more tokens than the tokenizer maximum length, `predict` truncates the text and returns a prediction.

**Proofs.**

- P-3.12-1 to P-3.12-4, P-3.12-7 to P-3.12-9. Evidence: test.
- P-3.12-5, P-3.12-6, P-3.12-10. Argument: `load` checks that `labels.json` exists, that its labels and tags are unique, and that their counts equal the last dimensions of `intent_logits` and `slot_logits`, before it returns `Ok`.
- P-3.12-11. Evidence: test.
