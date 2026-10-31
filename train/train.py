# Copyright 2026 Louis Lang
"""Fine-tunes the System One model on a training set file and exports it to ONNX."""

import argparse
import json
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import NotRequired, TypedDict

import torch
from torch import nn
from torch.nn import functional
from transformers import PreTrainedModel

IGNORE = -100
"""The label that the loss skips."""


@dataclass(frozen=True)
class Args:
    """The command-line options."""

    data: Path
    """The training set file."""
    out: Path
    """The folder for the model files."""
    device: str
    """`cpu` or `cuda`."""
    max_vram_gb: float | None
    """The GPU memory limit in GB, or `None` for no limit."""


def parse_args(argv: Sequence[str]) -> Args:
    """Read the options from the command-line arguments `argv`."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--device", choices=["cpu", "cuda"], required=True)
    parser.add_argument("--max-vram-gb", type=float)
    options = parser.parse_args(argv)
    return Args(options.data, options.out, options.device, options.max_vram_gb)


class Span(TypedDict):
    """A slot value at the character offsets `[start, end)` of the text."""

    slot: str
    start: int
    end: int


# `not` is a keyword, so only the functional syntax can name it.
Example = TypedDict(
    "Example",
    {
        "context": str,
        "text": str,
        "intent": NotRequired[str],
        "not": NotRequired[str],
        "spans": list[Span] | None,
    },
)
"""One labeled transcript with its context."""


class TrainingSet(TypedDict):
    """The fields of the training set file that the script reads."""

    labels: list[str]
    tags: list[str]
    examples: list[Example]


def load_set(path: Path) -> TrainingSet:
    """Read the training set file at `path` and check the label of each example."""
    training_set: TrainingSet = json.loads(path.read_text(encoding="utf-8"))
    for index, example in enumerate(training_set["examples"]):
        if ("intent" in example) == ("not" in example):
            message = f"example {index} needs exactly one of intent and not"
            raise ValueError(message)
        label = example.get("intent", example.get("not"))
        if label not in training_set["labels"]:
            message = f"example {index} has the unknown label {label}"
            raise ValueError(message)
    return training_set


def align(
    spans: list[Span] | None,
    offsets: Sequence[tuple[int, int]],
    segment_ids: Sequence[int],
    tags: list[str],
) -> list[int]:
    """Give the tag index of each token, or `IGNORE` for a context or special token."""
    labels: list[int] = []
    current = 0
    tagged = -1
    for (start, end), segment in zip(offsets, segment_ids, strict=True):
        # A fast tokenizer gives each special token the empty offset (0, 0).
        if spans is None or segment == 0 or start == end:
            labels.append(IGNORE)
            continue
        while current < len(spans) and spans[current]["end"] <= start:
            current += 1
        if current == len(spans) or start < spans[current]["start"]:
            labels.append(tags.index("O"))
            continue
        prefix = "I" if tagged == current else "B"
        slot = spans[current]["slot"]
        labels.append(tags.index(f"{prefix}-{slot}"))
        tagged = current
    return labels


class JointModel(nn.Module):
    """The encoder with an intent head on the first token and a slot head on each."""

    def __init__(self, base: PreTrainedModel, label_count: int, tag_count: int) -> None:
        """Put heads for `label_count` labels and `tag_count` tags on `base`."""
        super().__init__()
        self.base = base
        self.intent_head = nn.Linear(base.config.hidden_size, label_count)
        self.slot_head = nn.Linear(base.config.hidden_size, tag_count)

    def forward(
        self,
        input_ids: torch.Tensor,
        attention_mask: torch.Tensor,
        token_type_ids: torch.Tensor,
    ) -> tuple[torch.Tensor, torch.Tensor]:
        """Give the intent logits and the slot logits of each token."""
        hidden: torch.Tensor = self.base(
            input_ids=input_ids,
            attention_mask=attention_mask,
            token_type_ids=token_type_ids,
        ).last_hidden_state
        intent_logits: torch.Tensor = self.intent_head(hidden[:, 0])
        slot_logits: torch.Tensor = self.slot_head(hidden)
        return intent_logits, slot_logits


class Batch(TypedDict):
    """The targets of a group of examples."""

    labels: torch.Tensor
    """The label index of each example."""
    negated: torch.Tensor
    """`True` for a `not` example."""
    tags: torch.Tensor
    """The tag index of each token, or `IGNORE`."""


def loss(
    intent_logits: torch.Tensor, slot_logits: torch.Tensor, batch: Batch
) -> torch.Tensor:
    """Give the mean intent loss plus the mean slot loss over the tokens with a tag."""
    log_probabilities = intent_logits.log_softmax(-1)
    chosen = functional.one_hot(batch["labels"], intent_logits.shape[-1]).bool()
    # log(1 - p(k)) as the log of the sum of the others, which is finite at p(k) = 1.
    others = log_probabilities.masked_fill(chosen, -torch.inf).logsumexp(-1)
    picked = torch.where(batch["negated"], others, log_probabilities[chosen])
    slot_sum = functional.cross_entropy(
        slot_logits.flatten(0, 1),
        batch["tags"].flatten(),
        ignore_index=IGNORE,
        reduction="sum",
    )
    # The mean over no tagged token is 0, not NaN.
    tagged = (batch["tags"] != IGNORE).sum().clamp(min=1)
    return slot_sum / tagged - picked.mean()


if __name__ == "__main__":
    parse_args(sys.argv[1:])
