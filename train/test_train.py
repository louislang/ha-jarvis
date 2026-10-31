# Copyright 2026 Louis Lang
"""Tests of the training script."""

from pathlib import Path

import pytest

from train import Args, parse_args

REQUIRED = ["--data", "a.json", "--out", "m", "--device", "cpu"]


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
