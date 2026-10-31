# Copyright 2026 Louis Lang
"""Tests of the training script."""

import json
from pathlib import Path
from typing import TYPE_CHECKING

import onnxruntime
import pytest
import torch
from torch.nn import functional
from transformers import (
    AutoConfig,
    BertModel,
    BertTokenizerFast,
    PretrainedConfig,
    PreTrainedModel,
    PreTrainedTokenizerFast,
)

from train import (
    IGNORE,
    Args,
    Batch,
    Example,
    JointModel,
    Span,
    align,
    export,
    load_set,
    load_tokenizer,
    loss,
    main,
    parse_args,
    train,
)

if TYPE_CHECKING:
    from collections.abc import Callable

REQUIRED = ["--data", "a.json", "--out", "m", "--device", "cpu"]

TAGS = ["O", "B-area", "I-area"]

# Tokens of "[CLS] area : living room [SEP] turn on the living room lights [SEP]".
OFFSETS = [
    (0, 0),
    (0, 4),
    (4, 5),
    (6, 12),
    (13, 17),
    (0, 0),
    (0, 4),
    (5, 7),
    (8, 11),
    (12, 18),
    (19, 23),
    (24, 30),
    (0, 0),
]
SEGMENT_IDS = [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1]
LIVING_ROOM: list[Span] = [{"slot": "area", "start": 12, "end": 23}]

INPUTS = ["input_ids", "attention_mask", "token_type_ids"]

OFFICE = "area: office | habits: none"

LIGHTS: list[Example] = [
    {
        "context": f"area: {area} | habits: none",
        "text": f"turn on the {area} lights",
        "intent": "lights_on",
        "spans": [{"slot": "area", "start": 12, "end": 12 + len(area)}],
    }
    for area in ["kitchen", "office", "bedroom", "living room"]
]

UNKNOWN: list[Example] = [
    {"context": OFFICE, "text": text, "intent": "unknown", "spans": None}
    for text in ["what time is it", "tell me a joke", "hello there"]
]

NOT_LIGHTS: list[Example] = [
    {"context": OFFICE, "text": text, "not": "lights_on", "spans": []}
    for text in ["turn on the fan", "play some music", "open the garage"]
]

TEN = LIGHTS + UNKNOWN + NOT_LIGHTS

PASSES = 2
"""The epochs of the set of `set_path`."""

BATCHES = 3
"""The batches of at most 4 in the set of `set_path`."""

SPECIAL_TOKENS = ["[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]"]

torch.manual_seed(0)
torch.use_deterministic_algorithms(mode=True)


def training_set(examples: list[Example]) -> dict[str, object]:
    """Give a training set with two labels and the `area` tags."""
    return {
        "base_model": "microsoft/MiniLM-L12-H384-uncased",
        "epochs": 1,
        "batch_size": 8,
        "labels": ["unknown", "lights_on"],
        "tags": TAGS,
        "examples": examples,
    }


def write_set(tmp_path: Path, examples: list[Example]) -> Path:
    """Write a training set file with `examples` and give its path."""
    path = tmp_path / "set.json"
    path.write_text(json.dumps(training_set(examples)), encoding="utf-8")
    return path


def tiny_config(vocab_size: int) -> PretrainedConfig:
    """Give the configuration of a BERT with one small layer."""
    config: PretrainedConfig = AutoConfig.for_model(
        "bert",
        vocab_size=vocab_size,
        hidden_size=8,
        num_hidden_layers=1,
        num_attention_heads=1,
        intermediate_size=8,
    )
    return config


@pytest.fixture(scope="module")
def set_path(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """Write a random tiny BERT, a WordPiece tokenizer of `TEN` and a set of `TEN`."""
    folder = tmp_path_factory.mktemp("base")
    (folder / "vocab.txt").write_text("\n".join(SPECIAL_TOKENS), encoding="utf-8")
    empty: PreTrainedTokenizerFast = BertTokenizerFast.from_pretrained(folder)
    # `train_new_from_iterator` has no type hints.
    retrain: Callable[[list[str], int], PreTrainedTokenizerFast] = (
        empty.train_new_from_iterator
    )
    tokenizer = retrain([f"{e['context']} {e['text']}" for e in TEN], 1000)
    tokenizer.save_pretrained(folder)
    # The base class gives the typed constructor that `BertModel` lacks.
    encoder: type[PreTrainedModel] = BertModel
    encoder(tiny_config(len(tokenizer))).save_pretrained(folder)
    path = folder / "set.json"
    content = training_set(TEN) | {
        "base_model": str(folder),
        "epochs": PASSES,
        "batch_size": 4,
    }
    path.write_text(json.dumps(content), encoding="utf-8")
    return path


@pytest.fixture(scope="module")
def exported(
    set_path: Path, tmp_path_factory: pytest.TempPathFactory
) -> tuple[JointModel, PreTrainedTokenizerFast, Path]:
    """Train on the set of `set_path`, export it and give the model folder."""
    training_set = load_set(set_path)
    model = train(training_set, "cpu", None)
    tokenizer = load_tokenizer(training_set["base_model"])
    out = tmp_path_factory.mktemp("model")
    export(model, tokenizer, training_set, out)
    return model, tokenizer, out


def test_ac_3_5_1_parse_args_returns_values() -> None:
    """The three options give their values."""
    assert parse_args(REQUIRED) == Args(Path("a.json"), Path("m"), "cpu", None, None)


def test_ac_3_5_2_parse_args_rejects_gpu() -> None:
    """A device other than `cpu` or `cuda` is an error."""
    with pytest.raises(SystemExit):
        parse_args(["--data", "a.json", "--out", "m", "--device", "gpu"])


def test_ac_3_5_9_parse_args_reads_max_vram() -> None:
    """`--max-vram-gb` gives its number, and its absence gives `None`."""
    args = parse_args([*REQUIRED, "--max-vram-gb", "1.0"])
    assert args == Args(Path("a.json"), Path("m"), "cpu", 1.0, None)
    assert parse_args(REQUIRED).max_vram_gb is None


def test_ac_3_6_1_load_set_returns_valid_set(tmp_path: Path) -> None:
    """A valid file gives its training set."""
    examples: list[Example] = [
        {
            "context": "area: kitchen | habits: none",
            "text": "turn on the kitchen lights",
            "intent": "lights_on",
            "spans": [{"slot": "area", "start": 12, "end": 19}],
        },
        {
            "context": "area: office | habits: none",
            "text": "hi",
            "not": "lights_on",
            "spans": None,
        },
    ]
    assert load_set(write_set(tmp_path, examples)) == training_set(examples)


def test_ac_3_6_2_load_set_rejects_intent_and_not(tmp_path: Path) -> None:
    """An example with `intent` and `not` gives an error with its index."""
    examples: list[Example] = [
        {"context": "", "text": "hi", "intent": "unknown", "spans": None},
        {
            "context": "",
            "text": "hi",
            "intent": "unknown",
            "not": "lights_on",
            "spans": None,
        },
    ]
    with pytest.raises(ValueError, match="example 1"):
        load_set(write_set(tmp_path, examples))


def test_ac_3_6_3_load_set_rejects_unknown_label(tmp_path: Path) -> None:
    """A label that is not in `labels` gives an error with the label."""
    examples: list[Example] = [
        {"context": "", "text": "hi", "not": "fan_off", "spans": None}
    ]
    with pytest.raises(ValueError, match="fan_off"):
        load_set(write_set(tmp_path, examples))


def test_ac_3_6_4_align_tags_span_tokens() -> None:
    """The two tokens of "living room" get `B-area` and `I-area`, the others `O`."""
    labels = align(LIVING_ROOM, OFFSETS, SEGMENT_IDS, TAGS)
    assert labels[6:12] == [0, 0, 0, 1, 2, 0]


def test_ac_3_6_5_align_ignores_context_tokens() -> None:
    """Context and special tokens get `-100`, also at the offsets of a span."""
    labels = align(LIVING_ROOM, OFFSETS, SEGMENT_IDS, TAGS)
    assert labels[:6] == [-100] * 6
    assert labels[12:] == [-100]


def test_ac_3_6_6_align_without_spans_ignores_all() -> None:
    """`spans = None` gives `-100` for each token."""
    assert align(None, OFFSETS, SEGMENT_IDS, TAGS) == [-100] * len(OFFSETS)


def intent_loss(logit: float, *, negated: bool) -> float:
    """Give the loss of one example of label 1 when label 1 has the logit `logit`."""
    batch: Batch = {
        "labels": torch.tensor([1]),
        "negated": torch.tensor([negated]),
        "tags": torch.tensor([[IGNORE]]),
    }
    return loss(torch.tensor([[0.0, logit, 0.5]]), torch.zeros(1, 1, 3), batch).item()


def test_ac_3_7_1_model_gives_head_shapes() -> None:
    """A batch of 2 gives logits for 5 labels and for 7 tags on each token."""
    encoder: type[PreTrainedModel] = BertModel
    model = JointModel(encoder(tiny_config(16)), 5, 7)
    input_ids = torch.randint(16, (2, 4))
    intent_logits, slot_logits = model(
        input_ids, torch.ones_like(input_ids), torch.zeros_like(input_ids)
    )
    assert intent_logits.shape == (2, 5)
    assert slot_logits.shape == (2, 4, 7)


def test_ac_3_7_2_not_loss_falls_with_probability() -> None:
    """A lower `p(k)` gives a lower loss for an example with `not = k`."""
    assert intent_loss(-1.0, negated=True) < intent_loss(1.0, negated=True)


def test_ac_3_7_3_ignored_tags_add_no_slot_loss() -> None:
    """With only `-100` tags, the loss is the intent cross entropy."""
    intent_logits = torch.randn(2, 3)
    labels = torch.tensor([0, 2])
    batch: Batch = {
        "labels": labels,
        "negated": torch.tensor([False, False]),
        "tags": torch.full((2, 4), IGNORE),
    }
    expected = functional.cross_entropy(intent_logits, labels).item()
    total = loss(intent_logits, torch.randn(2, 4, 3), batch).item()
    assert total == pytest.approx(expected)


def test_ac_3_7_4_intent_loss_falls_as_probability_rises() -> None:
    """A higher `p(k)` gives a lower loss for an example with `intent = k`."""
    assert intent_loss(1.0, negated=False) < intent_loss(-1.0, negated=False)


def test_ac_3_8_1_main_writes_model_folder(set_path: Path, tmp_path: Path) -> None:
    """A CPU run of the script writes exactly the three model files."""
    main(["--data", str(set_path), "--out", str(tmp_path), "--device", "cpu"])
    names = sorted(path.name for path in tmp_path.iterdir())
    assert names == ["labels.json", "model.onnx", "tokenizer.json"]


def test_ac_3_8_2_onnx_matches_torch(
    exported: tuple[JointModel, PreTrainedTokenizerFast, Path],
) -> None:
    """ONNX Runtime and PyTorch give the same logits within 0.0001."""
    model, tokenizer, out = exported
    # A pair longer than the export sample also checks the dynamic sequence axis.
    encoding = tokenizer(
        ["area: kitchen | habits: none"] * 2,
        ["turn on the kitchen lights now", "hi"],
        padding=True,
        return_tensors="np",
    )
    session = onnxruntime.InferenceSession(out / "model.onnx")
    actual = session.run(None, {name: encoding[name] for name in INPUTS})
    with torch.no_grad():
        expected = model(*(torch.from_numpy(encoding[name]) for name in INPUTS))
    for onnx_logits, torch_logits in zip(actual, expected, strict=True):
        torch.testing.assert_close(
            torch.from_numpy(onnx_logits), torch_logits, atol=1e-4, rtol=0
        )


def test_ac_3_8_3_labels_file_keeps_set_order(
    set_path: Path, exported: tuple[JointModel, PreTrainedTokenizerFast, Path]
) -> None:
    """`labels.json` has the labels and the tags of the set, in order."""
    training_set = load_set(set_path)
    content = json.loads((exported[2] / "labels.json").read_text(encoding="utf-8"))
    assert content == {"labels": training_set["labels"], "tags": training_set["tags"]}


def record_batches(monkeypatch: pytest.MonkeyPatch) -> list[Batch]:
    """Make `train` record each batch that it gives to `loss`, and give the list."""
    batches: list[Batch] = []

    def counted(
        intent_logits: torch.Tensor, slot_logits: torch.Tensor, batch: Batch
    ) -> torch.Tensor:
        """Record `batch` and give its loss."""
        batches.append(batch)
        return loss(intent_logits, slot_logits, batch)

    monkeypatch.setattr("train.loss", counted)
    return batches


def test_ac_3_8_4_two_epochs_run_two_passes(
    set_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """With 2 epochs, the loop gives the loss of each of the 3 batches twice."""
    batches = record_batches(monkeypatch)
    train(load_set(set_path), "cpu", None)
    assert len(batches) == PASSES * BATCHES


def test_epochs_option_replaces_set_epochs(
    set_path: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """With `--epochs 1`, a set with 2 epochs gives the loss of each batch once."""
    batches = record_batches(monkeypatch)
    main(
        [
            "--data",
            str(set_path),
            "--out",
            str(tmp_path),
            "--device",
            "cpu",
            "--epochs",
            "1",
        ]
    )
    assert len(batches) == BATCHES
