# Copyright 2026 Louis Lang
"""Fine-tunes the System One model on a training set file and exports it to ONNX."""

import argparse
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path


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


if __name__ == "__main__":
    parse_args(sys.argv[1:])
