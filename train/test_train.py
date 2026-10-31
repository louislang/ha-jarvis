# Copyright 2026 Louis Lang
"""Tests of the training script."""

import json
from pathlib import Path

import pytest
import torch
from torch.nn import functional
from transformers import AutoConfig, BertModel, PreTrainedModel

from train import (
    IGNORE,
    Args,
    Batch,
    Example,
    JointModel,
    Span,
    align,
    load_set,
    loss,
    parse_args,
)

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


def test_ac_3_5_1_parse_args_returns_values() -> None:
    """The three options give their values."""
    assert parse_args(REQUIRED) == Args(Path("a.json"), Path("m"), "cpu", None)


def test_ac_3_5_2_parse_args_rejects_gpu() -> None:
    """A device other than `cpu` or `cuda` is an error."""
    with pytest.raises(SystemExit):
        parse_args(["--data", "a.json", "--out", "m", "--device", "gpu"])


def test_ac_3_5_9_parse_args_reads_max_vram() -> None:
    """`--max-vram-gb` gives its number, and its absence gives `None`."""
    args = parse_args([*REQUIRED, "--max-vram-gb", "1.0"])
    assert args == Args(Path("a.json"), Path("m"), "cpu", 1.0)
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
    config = AutoConfig.for_model(
        "bert",
        vocab_size=16,
        hidden_size=8,
        num_hidden_layers=1,
        num_attention_heads=1,
        intermediate_size=8,
    )
    # The base class gives the typed constructor that `BertModel` lacks.
    encoder: type[PreTrainedModel] = BertModel
    model = JointModel(encoder(config), 5, 7)
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
