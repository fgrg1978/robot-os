#!/usr/bin/env python3
"""F18 — Sign an OTA kernel image with Ed25519.

Reads the raw kernel .BIN and writes a matching .SIG file containing
the `FirmwareSignature` header expected by crates/crypto/src/ed25519.rs:

    Offset  Size  Field
    0       4     magic "RSIG"
    4       1     algorithm (0 = Ed25519)
    5       32    public key (raw)
    37      64    signature (raw)
    101     4     payload size (u32 LE)

Total header = 105 bytes.

Usage:
    python3 tools/sign_ota.py path/to/kernel.bin [--priv tools/keys/dev_priv.bin]

Outputs:
    path/to/kernel.sig  (same basename, .sig extension)
"""

import argparse
import hashlib
import struct
import sys
from pathlib import Path

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives import serialization
except ImportError:
    print("error: install the 'cryptography' package: pip install cryptography", file=sys.stderr)
    sys.exit(1)

SIG_MAGIC = b"RSIG"
SIG_ALGORITHM_ED25519 = 0
ED25519_PUBLIC_KEY_SIZE = 32
ED25519_SIGNATURE_SIZE = 64
SIG_HEADER_SIZE = 4 + 1 + ED25519_PUBLIC_KEY_SIZE + ED25519_SIGNATURE_SIZE + 4


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("image", type=Path, help="Raw kernel binary to sign")
    p.add_argument(
        "--priv",
        type=Path,
        default=Path(__file__).parent / "keys" / "dev_priv.bin",
        help="Ed25519 private key (raw 32-byte seed)",
    )
    p.add_argument(
        "--out",
        type=Path,
        default=None,
        help="Output .sig path (default: <image>.sig replacing .bin extension)",
    )
    args = p.parse_args()

    if not args.priv.exists():
        print(f"error: private key not found: {args.priv}", file=sys.stderr)
        print("hint: run tools/gen_dev_key.py first.", file=sys.stderr)
        return 1

    priv_raw = args.priv.read_bytes()
    if len(priv_raw) != ED25519_PUBLIC_KEY_SIZE:
        print("error: private key is not 32 bytes", file=sys.stderr)
        return 1

    priv = Ed25519PrivateKey.from_private_bytes(priv_raw)
    pub_raw = priv.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )

    image = args.image.read_bytes()
    signature = priv.sign(image)

    assert len(pub_raw) == ED25519_PUBLIC_KEY_SIZE
    assert len(signature) == ED25519_SIGNATURE_SIZE

    header = bytearray()
    header += SIG_MAGIC
    header.append(SIG_ALGORITHM_ED25519)
    header += pub_raw
    header += signature
    header += struct.pack("<I", len(image))
    assert len(header) == SIG_HEADER_SIZE

    out = args.out
    if out is None:
        out = args.image.with_suffix(".sig")
    out.write_bytes(bytes(header))

    digest = hashlib.sha256(image).hexdigest()
    print(f"✓ signed {args.image} ({len(image)} bytes)")
    print(f"  sha256 : {digest}")
    print(f"  sig out: {out} ({SIG_HEADER_SIZE} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
