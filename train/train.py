# Copyright 2026 Louis Lang
"""Fine-tunes the System One model on a training set file and exports it to ONNX."""

import argparse
import json
import sys
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import NotRequired, TypedDict

import torch
from torch import nn
from torch.nn import functional
from transformers import (
    AutoModel,
    AutoTokenizer,
    PreTrainedModel,
    PreTrainedTokenizerFast,
)

IGNORE = -100
"""The label that the loss skips."""

INPUTS = ["input_ids", "attention_mask", "token_type_ids"]
"""The input names of the model, in `forward` order."""


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

    base_model: str
    epochs: int
    batch_size: int
    labels: list[str]
    tags: list[str]
    examples: list[Example]


def named_label(example: Example) -> str:
    """Give the label that `intent` or `not` names."""
    return example["intent"] if "intent" in example else example["not"]


def load_set(path: Path) -> TrainingSet:
    """Read the training set file at `path` and check the label of each example."""
    training_set: TrainingSet = json.loads(path.read_text(encoding="utf-8"))
    for index, example in enumerate(training_set["examples"]):
        if ("intent" in example) == ("not" in example):
            message = f"example {index} needs exactly one of intent and not"
            raise ValueError(message)
        label = named_label(example)
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


def load_tokenizer(base_model: str) -> PreTrainedTokenizerFast:
    """Load the fast tokenizer of the model `base_model`."""
    # `AutoTokenizer.from_pretrained` has no type hints.
    load: Callable[[str], PreTrainedTokenizerFast] = AutoTokenizer.from_pretrained
    return load(base_model)


def encode(
    tokenizer: PreTrainedTokenizerFast,
    training_set: TrainingSet,
    examples: list[Example],
    device: str,
) -> tuple[list[torch.Tensor], Batch]:
    """Give the model inputs in `INPUTS` order and the targets of `examples`."""
    encoding = tokenizer(
        [example["context"] for example in examples],
        [example["text"] for example in examples],
        padding=True,
        return_offsets_mapping=True,
        return_tensors="pt",
    ).to(device)
    rows = zip(
        examples, encoding["offset_mapping"], encoding["token_type_ids"], strict=True
    )
    tags = [
        align(
            example["spans"], offsets.tolist(), segments.tolist(), training_set["tags"]
        )
        for example, offsets, segments in rows
    ]
    labels = [training_set["labels"].index(named_label(e)) for e in examples]
    targets: Batch = {
        "labels": torch.tensor(labels, device=device),
        "negated": torch.tensor(["not" in e for e in examples], device=device),
        "tags": torch.tensor(tags, device=device),
    }
    return [encoding[name] for name in INPUTS], targets


def limit_vram(device: str, max_vram_gb: float | None) -> None:
    """Make PyTorch fail with "CUDA out of memory" above `max_vram_gb` on `cuda`."""
    if device == "cuda" and max_vram_gb is not None:
        total = torch.cuda.get_device_properties(device).total_memory
        torch.cuda.set_per_process_memory_fraction(max_vram_gb * 2**30 / total)


def train(
    training_set: TrainingSet, device: str, max_vram_gb: float | None
) -> JointModel:
    """Fine-tune the base model on `training_set` with AdamW on `device`."""
    limit_vram(device, max_vram_gb)
    tokenizer = load_tokenizer(training_set["base_model"])
    base = AutoModel.from_pretrained(training_set["base_model"])
    labels, tags = training_set["labels"], training_set["tags"]
    model = JointModel(base, len(labels), len(tags)).to(device)
    optimizer = torch.optim.AdamW(model.parameters())
    examples, batch_size = training_set["examples"], training_set["batch_size"]
    batches = [
        encode(tokenizer, training_set, examples[start : start + batch_size], device)
        for start in range(0, len(examples), batch_size)
    ]
    epochs = training_set["epochs"]
    model.train()
    for _epoch in range(epochs):
        for inputs, batch in batches:
            intent_logits, slot_logits = model(*inputs)
            torch.autograd.backward(loss(intent_logits, slot_logits, batch))
            optimizer.step()
            optimizer.zero_grad()
    return model


def export(
    model: JointModel,
    tokenizer: PreTrainedTokenizerFast,
    training_set: TrainingSet,
    out: Path,
) -> None:
    """Write `model.onnx`, `tokenizer.json` and `labels.json` to the folder `out`."""
    out.mkdir(parents=True, exist_ok=True)
    example = training_set["examples"][0]
    sample = tokenizer(example["context"], example["text"], return_tensors="pt")
    sample = sample.to(model.intent_head.weight.device)
    axes = {name: {0: "batch", 1: "sequence"} for name in [*INPUTS, "slot_logits"]}
    model.eval()
    torch.onnx.export(
        model,
        tuple(sample[name] for name in INPUTS),
        out / "model.onnx",
        input_names=INPUTS,
        output_names=["intent_logits", "slot_logits"],
        dynamic_axes=axes | {"intent_logits": {0: "batch"}},
    )
    tokenizer.backend_tokenizer.save(str(out / "tokenizer.json"))
    labels = {"labels": training_set["labels"], "tags": training_set["tags"]}
    (out / "labels.json").write_text(json.dumps(labels), encoding="utf-8")


def main(argv: Sequence[str]) -> None:
    """Train on the training set file of `argv` and write the model folder."""
    args = parse_args(argv)
    training_set = load_set(args.data)
    model = train(training_set, args.device, args.max_vram_gb)
    tokenizer = load_tokenizer(training_set["base_model"])
    export(model, tokenizer, training_set, args.out)


if __name__ == "__main__":
    main(sys.argv[1:])
