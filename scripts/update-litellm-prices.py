#!/usr/bin/env python3
"""Regenerate data/litellm-prices.json from a LiteLLM price file (dev tool, stdlib only).

Usage:
  scripts/update-litellm-prices.py <litellm.json> [--out FILE] [--generated YYYY-MM-DD]
  scripts/update-litellm-prices.py --download [--out FILE]

Rules (spec 002, R5 and R6):
  - keep entries with mode chat or responses from providers anthropic, openai, gemini, dashscope
  - skip entries without both input and output cost, and entries whose input and output are both 0
    (LiteLLM does not model a per-token price for them; they stay unpriced, never $0.00)
  - skip, with a warning on stderr, entries with an invalid price (not a number, bool, negative,
    NaN or infinite) in any of the five fields used
  - key = id without its "<provider>/" prefix; on a collision the unprefixed key wins
  - value = [input, output, write_5m, write_1h, read], USD per million tokens
  - missing cache prices: Anthropic 1.25x / 2x / 0.1x input; others write 0, read = input
  - the output records the sha256 of the input bytes (not its path) and the upstream URL label
"""
import argparse
import datetime
import hashlib
import json
import math
import sys
import urllib.request

URL = ("https://raw.githubusercontent.com/BerriAI/litellm/main/"
       "model_prices_and_context_window.json")
PROVIDERS = {"anthropic", "openai", "gemini", "dashscope"}
MODES = {"chat", "responses"}


def per_million(v):
    return round(v * 1e6, 6) + 0.0


def price(e, field):
    """A price field: None when absent, else a finite non-negative number (ValueError if not)."""
    v = e.get(field)
    if v is None:
        return None
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or v < 0:
        raise ValueError("invalid %s" % field)
    return v


def convert(e, provider):
    i, o = price(e, "input_cost_per_token"), price(e, "output_cost_per_token")
    w5 = price(e, "cache_creation_input_token_cost")
    w1 = price(e, "cache_creation_input_token_cost_above_1hr")
    rd = price(e, "cache_read_input_token_cost")
    if i is None or o is None or (i == 0 and o == 0):
        return None
    anth = provider == "anthropic"
    if w5 is None:
        w5 = 1.25 * i if anth else 0
    if w1 is None:
        w1 = 2 * i if anth else 0
    if rd is None:
        rd = 0.1 * i if anth else i
    return [per_million(x) for x in (i, o, w5, w1, rd)]


def build(src):
    models, bare_wins = {}, {}
    for key, e in src.items():
        if not isinstance(e, dict):
            continue
        provider = e.get("litellm_provider")
        if provider not in PROVIDERS or e.get("mode") not in MODES:
            continue
        try:
            row = convert(e, provider)
        except ValueError as err:
            print("skipping %s: %s" % (key, err), file=sys.stderr)
            continue
        if row is None:
            continue
        prefix = provider + "/"
        has_prefix = key.startswith(prefix)
        bare = key[len(prefix):] if has_prefix else key
        if bare in models and bare_wins[bare] and has_prefix:
            continue
        models[bare] = row
        bare_wins[bare] = not has_prefix
    return dict(sorted(models.items()))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("input", nargs="?", help="LiteLLM JSON file")
    ap.add_argument("--download", action="store_true", help="fetch " + URL)
    ap.add_argument("--out", default="data/litellm-prices.json")
    ap.add_argument("--generated", default=datetime.date.today().isoformat())
    a = ap.parse_args()
    if a.download == bool(a.input):
        ap.error("give either an input file or --download")
    if a.download:
        with urllib.request.urlopen(URL, timeout=60) as r:
            raw = r.read()
    else:
        with open(a.input, "rb") as f:
            raw = f.read()
    out = {
        "source": "BerriAI/litellm model_prices_and_context_window.json (" + URL + ")",
        "input_sha256": hashlib.sha256(raw).hexdigest(),
        "generated": a.generated,
        "models": build(json.loads(raw)),
    }
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(out, f, sort_keys=True, separators=(",", ":"), allow_nan=False)
        f.write("\n")
    print("wrote %s: %d models" % (a.out, len(out["models"])), file=sys.stderr)


if __name__ == "__main__":
    main()
