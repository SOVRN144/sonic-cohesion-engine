#!/usr/bin/env python3
"""Generate a deterministic WAV fixture for local SCE smoke runs."""

import argparse
import math
import struct
import wave
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate a sine WAV at a nominal level for smoke testing."
    )
    parser.add_argument("output", type=Path, help="Output WAV path.")
    parser.add_argument(
        "--sample-rate-hz",
        type=int,
        default=48_000,
        help="Sample rate in Hz (default: 48000).",
    )
    parser.add_argument(
        "--channels",
        type=int,
        default=2,
        choices=[1, 2],
        help="Channel count (default: 2).",
    )
    parser.add_argument(
        "--duration-sec",
        type=float,
        default=1.0,
        help="Signal duration in seconds (default: 1.0).",
    )
    parser.add_argument(
        "--frequency-hz",
        type=float,
        default=440.0,
        help="Sine frequency in Hz (default: 440.0).",
    )

    level = parser.add_mutually_exclusive_group()
    level.add_argument(
        "--peak-dbfs",
        type=float,
        help="Target peak level in dBFS. Default is -12.0 dBFS peak when omitted.",
    )
    level.add_argument(
        "--rms-dbfs",
        type=float,
        help="Target RMS level in dBFS for a sine wave (converted to peak).",
    )

    return parser.parse_args()


def amplitude_from_args(args: argparse.Namespace) -> float:
    if args.rms_dbfs is not None:
        amplitude = (10.0 ** (args.rms_dbfs / 20.0)) * math.sqrt(2.0)
    else:
        peak_dbfs = args.peak_dbfs if args.peak_dbfs is not None else -12.0
        amplitude = 10.0 ** (peak_dbfs / 20.0)

    if not (0.0 < amplitude <= 1.0):
        raise ValueError("level produces amplitude outside (0, 1].")
    return amplitude


def main() -> None:
    args = parse_args()
    amplitude = amplitude_from_args(args)
    sample_count = int(round(args.duration_sec * args.sample_rate_hz))
    args.output.parent.mkdir(parents=True, exist_ok=True)

    with wave.open(str(args.output), "wb") as wav:
        wav.setnchannels(args.channels)
        wav.setsampwidth(2)  # i16 PCM
        wav.setframerate(args.sample_rate_hz)

        for idx in range(sample_count):
            sample = math.sin(2.0 * math.pi * args.frequency_hz * idx / args.sample_rate_hz)
            sample *= amplitude
            i16_value = int(max(-1.0, min(1.0, sample)) * 32767.0)
            frame = struct.pack("<" + ("h" * args.channels), *([i16_value] * args.channels))
            wav.writeframesraw(frame)


if __name__ == "__main__":
    main()
