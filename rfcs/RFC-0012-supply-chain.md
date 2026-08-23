# RFC-0012: Supply Chain Hardening (SBOM + SLSA + Signed Releases)

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

Every PHANES release ships with a Software Bill of Materials
(CycloneDX), is built reproducibly, signed with Sigstore (cosign),
and publishes provenance attestations meeting SLSA Level 3. Brain
side ships pip-audit + `cyclonedx-bom` + `sigstore-python` signed
wheels. This satisfies EU CRA, US Executive Order 14028, and most
enterprise procurement supply-chain requirements.

## Motivation

EU Cyber Resilience Act (in force from 2027) and US EO 14028 (in
force) require SBOM, signed releases, and verifiable provenance for
software sold to government / large enterprise / EU consumers.
Without these, PHANES cannot be sold in those markets.

Supply-chain attacks (xz-utils 2024, SolarWinds, Log4Shell) further
motivate: an attacker compromising upstream affects every
downstream PHANES build unless we can verify what we're shipping
matches what we built from what the source said.

## Detailed design

### SBOM (Software Bill of Materials)

**Format:** CycloneDX 1.5 JSON. Why CycloneDX over SPDX: better
tooling in the Rust + Python ecosystem; OWASP-managed; widely
adopted.

**Tooling:**

- Kernel side: `cargo-cyclonedx` generates SBOM from Cargo.lock at
  build time.
- Brain side: `cyclonedx-bom` (Python tool) generates from
  `requirements.lock`.
- Combined SBOM: meta-SBOM listing both kernel and brain SBOMs,
  signed.

**Per-release artefacts:**

```
phanes-1.0.0/
├── kernel-qemu.bin
├── kernel-qemu.bin.sig             ← Ed25519 (RFC-0011)
├── kernel-vf2.bin
├── kernel-vf2.bin.sig
├── kernel-imx8mp.bin
├── kernel-imx8mp.bin.sig
├── sbom.cdx.json                    ← CycloneDX combined SBOM
├── provenance.intoto.jsonl          ← SLSA Level 3 provenance
├── signatures/
│   ├── sbom.sig                     ← cosign signature on SBOM
│   ├── provenance.sig
│   └── ...
└── checksums.txt.sig                ← cosign signature on the lot
```

### Reproducible builds

Goal: same commit produces byte-identical binaries.

Sources of non-determinism we eliminate:

| Source | Mitigation |
|--------|------------|
| Build timestamps embedded | `SOURCE_DATE_EPOCH` env from commit time |
| File ordering | `LC_ALL=C` + sorted file lists in build.rs |
| Path strings in panics / debug info | Stable paths via `--remap-path-prefix` |
| Cargo.lock floating versions | Pinned via Cargo.lock committed |
| build.rs random | None (audited; replace any RNG calls) |
| Random tmpfile names | `mktemp` with deterministic seed |

CI verification: `scripts/build-twice-compare.sh` runs the build
twice in clean containers and asserts byte-identical output. Failure
fails the release.

### SLSA Level 3 provenance

**SLSA** (Supply-chain Levels for Software Artefacts) — Google /
Linux Foundation framework. Level 3 requires:

- **Build platform integrity** — build runs in a hardened CI
  environment (GitHub-hosted runners; we audit their isolation).
- **Provenance generation** — every artefact has a signed
  attestation listing source, builder, materials.
- **Provenance non-falsifiability** — signed by the build platform's
  key, verifiable.

Implementation:

```yaml
# .github/workflows/release.yml
- uses: slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml@v1
  with:
    base64-subjects: "${{ steps.hashes.outputs.hashes }}"
    upload-assets: true
```

The action generates `provenance.intoto.jsonl`, signed by GitHub
OIDC + Rekor transparency log. Anyone can verify:

```bash
slsa-verifier verify-artifact phanes-1.0.0.tar.gz \
    --provenance phanes-1.0.0.intoto.jsonl \
    --source-uri github.com/phanes-project/phanes \
    --source-tag v1.0.0
```

### Sigstore signing

Every release artefact is signed with `cosign` (Sigstore). Public
verifiability via the Sigstore Rekor transparency log.

```bash
# Release CI
cosign sign --yes phanes-1.0.0/kernel-imx8mp.bin
# → produces .sig file + Rekor entry

# Anyone:
cosign verify-blob \
    --signature kernel-imx8mp.bin.sig \
    --certificate-identity 'release@phanes-project.org' \
    --certificate-oidc-issuer 'https://github.com/login/oauth' \
    kernel-imx8mp.bin
```

### Brain side (`phanes-brain`)

```bash
# In release CI:
pip-audit                                        # known CVE check
cyclonedx-py --requirements requirements.lock \
    --output-file phanes-brain-1.0.0.cdx.json
sigstore sign --output dist/phanes_brain-1.0.0-py3-none-any.whl
```

PyPI wheel is uploaded with PEP 740 attestations.

### Dependency policy

| Tier | Requirements |
|------|--------------|
| **Tier 1** (`crates/{ipc,sched,mm,ota,crypto,arch}`) | No new deps without RFC. Existing minimised. Auditing required. |
| **Tier 2** (`crates/{drivers,fs,net,behavior}`) | Deps allowed but reviewed. License must be Apache 2.0 / MIT / BSD / 0BSD. |
| **Tier 3** (`crates/{ml,camera,flight,nav}` etc.) | Pragmatic; same license filter. |
| **Brain** | Same license filter; pinned via `requirements.lock`. |

### CI gates

| Gate | Tool | Failure mode |
|------|------|--------------|
| License compliance | `cargo-deny` + `pip-licenses` | Block on GPL-incompat |
| CVE scan | `cargo-audit` + `pip-audit` | Block on advisory |
| Reproducibility | `scripts/build-twice-compare.sh` | Block on byte-divergence |
| SBOM generation | `cargo-cyclonedx`, `cyclonedx-bom` | Required artefact |
| Signature verification | `cosign verify` | Block on unsigned |

### OpenSSF Best Practices Badge

Phase 1 deliverable: passing badge. Phase 2: silver. Phase 3: gold.
Required for Linux Foundation incubation graduation.

Criteria mapped:

- Tests, CI, semver: ✅ have or trivially add
- Reproducible builds: this RFC
- SBOM: this RFC
- Signed releases: this RFC
- Vulnerability disclosure: RFC-0009 PSIRT
- Bug bounty: Phase 2 (RFC-0009)

### OpenChain ISO/IEC 5230 conformance

License-compliance management standard. We achieve via:

- License headers in every source file (auto-checked in CI).
- `LICENSE` + `NOTICE` at repo root.
- `cargo-deny` + `pip-licenses` enforcing allowed licenses.
- License inventory in SBOM.

## Drawbacks

- **CI complexity grows.** Reproducible-build verification doubles
  CI time. Mitigated by parallel matrix build.
- **Dependency review overhead.** Every new dep needs license + CVE
  + provenance review. We accept this; it's the discipline.
- **Sigstore depends on third-party (Rekor).** If Sigstore fails,
  releases stall. Mitigated by mirror to a self-hosted backup
  transparency log post-Phase 2.

## Rationale and alternatives

**Alternative A — SPDX instead of CycloneDX.** Equivalent SBOM
format. CycloneDX has better tooling in Rust+Python ecosystem;
small preference, not a make-or-break.

**Alternative B — sign with PGP.** Legacy. Sigstore is the modern
replacement; OpenSSF endorses.

**Alternative C — skip reproducible builds initially.** Tempting,
but EU CRA likely requires within 1–2 years. Worth doing now.

## Prior art

- **Linux kernel** — has reproducible build infrastructure
  (`KBUILD_BUILD_TIMESTAMP`).
- **Debian's reproducible-builds.org** project — extensive guides.
- **AWS s2n / Nitro Enclaves** — published SBOMs and SLSA.
- **CNCF projects** (Kubernetes, etc.) — SLSA + cosign standard.
- **NPM provenance** — same shape, JS ecosystem.

## Unresolved questions

- **Signing key custody.** Where does the cosign signing key live?
  Working assumption: GitHub OIDC keyless signing (no key custody;
  signed by ephemeral cert tied to GitHub identity). Phase 2: also
  hardware-backed signing for high-value releases.
- **SBOM update cadence.** Every release is obvious. What about
  dev branches? Working assumption: weekly automated SBOM run on
  `main`.
- **Audit log retention.** Rekor stores forever. Internal release
  metadata: 10 years (matches LTS support window).

## Future possibilities

- **Phase 3:** Self-hosted transparency log (mirror Rekor).
- **Phase 4:** Hardware-backed signing (HSM in CI runners).
- **Phase 4:** Reproducible-build verification by community
  (multiple independent rebuilders).
