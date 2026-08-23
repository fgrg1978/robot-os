# PHANES — Release Process

> **Audience:** PHANES maintainers preparing a public release.  
> **Pre-requisites:** RFC-0012 (supply chain), RFC-0016 (operational
> excellence), `CHANGELOG.md`.

This document is the per-release checklist for any `v1.x` PHANES
publication. It's also the script the GitHub Actions release
workflow follows (see `.github/workflows/release.yml` — pending in
W6 rest).

## Pre-flight (1 week out)

- [ ] **CHANGELOG.md** updated with the release version + date.
- [ ] **`crates/abi/CHANGELOG.md`** updated if any new `pub` items.
- [ ] **All RFCs marked `accepted`** that gate this release.
- [ ] **All host test suites green** on `main`:
  - regression-tests · ota-tests · abi-tests · cap-tests
  - topology-tests · sched-policy-tests · brain pytest
- [ ] **5 build configs clean** (qemu / no-ml / no-mmu / vf2 / k1):
  `cargo build --release` × 5
- [ ] **TLA+ TLC verification** passes on the three specs:
  `cap_table`, `topology_load`, `sched_aps`.
- [ ] **mdBook builds** without warnings.
- [ ] **QEMU smoke** (`make qemu` + `make qemu-full-smp`) prints the
  expected `[TOPO]` and `[APS]` lines.
- [ ] **`cargo deny check`** clean (license + advisories).
- [ ] **`cargo audit`** clean (no high+ CVEs in deps).

## Tag & build

```bash
# 1. Update version everywhere.
#    - top-level Cargo.toml (no version yet — workspace has none)
#    - crates/abi/Cargo.toml          v1.0.0 → next
#    - CHANGELOG.md headline
git commit -s -m "release: v1.0.0 (Phase 1 close)"

# 2. Tag with annotation.
git tag -a v1.0.0 -m "PHANES v1.0.0 — Phase 1 close"

# 3. Build all 5 configs into target/release/.
scripts/build.sh
```

## Verify reproducibility

PHANES targets reproducible builds (RFC-0012). Run a clean second
build and compare:

```bash
scripts/build-twice-compare.sh   # pending in W6 rest
```

Until that script lands, manual:

```bash
git clean -fdx
cargo build --release
sha256sum target/riscv64imac-unknown-none-elf/release/kernel
# Should match the first build's hash.
```

Mismatch ⇒ investigate `SOURCE_DATE_EPOCH`, `--remap-path-prefix`,
or platform-dependent build steps before publishing.

## SBOM generation

```bash
# CycloneDX SBOM for the kernel workspace.
cargo cyclonedx --format json --output-pattern bom.cdx.json

# CycloneDX SBOM for the brain (sibling repo).
cd ../phanes-brain
cyclonedx-py --output-file phanes-brain.cdx.json
```

The two SBOMs are bundled with the release artefact set:

```
phanes-v1.0.0/
├── kernel-qemu.bin
├── kernel-vf2.bin
├── kernel-k1.bin
├── kernel-no-ml.bin
├── kernel-no-mmu.bin
├── bom.cdx.json                    # combined SBOM
├── checksums.txt                   # sha256 of every binary
└── (signatures appear after the next step)
```

## Sigstore signing

```bash
# Sign every binary with Sigstore cosign (OIDC keyless).
for f in phanes-v1.0.0/*.bin phanes-v1.0.0/bom.cdx.json; do
    cosign sign --yes "$f"           # → produces $f.sig
done

# Sign the checksums manifest.
cosign sign --yes phanes-v1.0.0/checksums.txt
```

The signatures land in the Rekor transparency log; anyone can verify
without trusting the maintainer's private key:

```bash
cosign verify-blob \
    --signature kernel-vf2.bin.sig \
    --certificate-identity 'release@phanes-project.org' \
    --certificate-oidc-issuer 'https://github.com/login/oauth' \
    kernel-vf2.bin
```

## SLSA Level 3 provenance

The GitHub Actions release workflow uses the
`slsa-framework/slsa-github-generator` action to attach a signed
provenance attestation to each binary. See `.github/workflows/
release.yml` (W6 rest deliverable).

## Publish

```bash
# Push the tag.
git push origin v1.0.0

# Create the GitHub release with the artefact set.
gh release create v1.0.0 \
    --title "PHANES v1.0.0 — Phase 1 close" \
    --notes-file CHANGELOG.md \
    phanes-v1.0.0/*

# Publish robot_os_abi to crates.io (the only crate published
# from this repo for v1.0; the kernel + other crates stay
# workspace-internal until Phase 2 platform-portability).
cd crates/abi
cargo publish --dry-run
cargo publish
```

## Post-release

- [ ] **CHANGELOG.md**: open a new `[Unreleased]` section for the
  next development cycle.
- [ ] **Bump `ABI_VERSION`** only at major releases (v2.0+).
- [ ] **Announce** on the project mailing list + GitHub Discussions +
  whichever Linux Foundation incubator board applies.
- [ ] **Update the OpenSSF Best Practices Badge** entry for any new
  criteria met.

## LF incubation milestone (one-shot, at v1.0.0)

The PHANES Linux Foundation incubation application is submitted
**concurrent with v1.0.0**. Required artefacts:

- This `RELEASE.md` (process documented).
- `CHANGELOG.md` v1.0.0 entry.
- `CONTRIBUTING.md` + `CODE_OF_CONDUCT.md` (CC v2.1).
- All RFCs `accepted` or `implemented`.
- ≥ 5 external contributors (Phase 2+; not gating v1.0).
- OpenSSF Best Practices Badge "passing" tier (W6 rest deliverable).

See `rfcs/RFC-0009-governance.md` for the full incubation roadmap.

## Hotfix procedure

Critical security / safety fix on a published release:

```bash
git checkout v1.0.0
git checkout -b release/v1.0.x
# apply fix
git commit -s -m "v1.0.1: fix CVE-2026-XXXXX"
git tag -a v1.0.1 -m "PHANES v1.0.1 — security fix"
# run the standard pre-flight + tag/build/sign/publish flow
```

Critical CVEs ship within 14 days of advisory (RFC-0016 SLA).
