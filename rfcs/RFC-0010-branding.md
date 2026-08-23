# RFC-0010: Branding — PHANES

> **Status:** accepted  
> **Authors:** Fernando Rodriguez  
> **Created:** 2026-05-10  
> **Last updated:** 2026-05-10

## Summary

The project is named **PHANES**. PHANES is the Orphic primordial
deity of light and creation — "the first mind that gives form to
everything". Six letters, two syllables, pronounceable globally,
trademark-clear in the OS / kernel space, with no major collisions
in the open-source software ecosystem.

## Motivation

A name that gets cited in regulation, standards, and academic
papers must be:

1. Memorable enough to stick.
2. Searchable enough to find.
3. Trademark-clean enough to defend.
4. Free of negative associations.
5. Aligned with the technical positioning ("OS as the head/brain of
   the system" — the user's framing).

The shortlist evaluated and rejected included candidates that
collided with existing significant projects:

| Rejected | Reason |
|----------|--------|
| Talos | Sidero Labs Talos Linux (Kubernetes OS, large) + Cisco Talos |
| Mimir | Grafana Mimir (Prometheus storage, large) |
| Multivac | Multivac SE (German packaging giant, 60 yrs) |
| Daedalus | Devuan Daedalus + multiple Linux distros |
| Tvashtar | Tavashtr OS (Debian-based, exists) |
| Metis | Metis Linux + Axelera Metis + others |
| Hephaestus | PureOS codename + multiple smaller |
| Krytos / Kryptos | Kry10 (seL4-based commercial OS — direct competitor) |
| Aether / Aetheros | Aetheros Inc, ARCHAI AetherOS |
| Veros | Vero financial platform |
| Enki | Enki Home, Enki Editor, Enki learn-to-code |
| Nabu | Xiaomi Pad 5 codename |
| Ferrox | ferrox.dev (Rust process manager — same ecosystem) |
| Cordis | Cordis Corp (J&J cardiac devices) |
| Astryx | Multiple companies (astryx.tech, .ca, .org) |

## Detailed design

### The name — PHANES

**Mythological background.** Phanes (Greek: Φάνης, "bringer of
light") is the primordial deity in Orphic cosmogony. In the Orphic
hymns, Phanes hatches from the cosmic egg and **gives form to all
that exists** — he is literally the first organising mind of the
universe. The name comes from the verb φαίνω (*phaínō*), "to make
appear, to bring into the light".

Why this fits an OS:

- **Phanes gives form to systems** — exactly what an OS does for
  hardware.
- **Bringer of light** — perception, intelligence, illumination. AI
  positioning.
- **First mind** — the kernel as the organising principle of the
  robot.
- **Cult-knowledge upside** — those who know mythology recognise it
  immediately and become embassadors. Those who don't, learn one
  cool fact.

### Pronunciation

- English: **FAY-nees** (`/ˈfeɪniːz/`)
- Spanish: FA-nes
- German: FA-nes
- Japanese: ファネス (fa-ne-su)
- Mandarin: 法尼斯 (fǎ ní sī) approximate

Two syllables, no consonant clusters, no exotic phonemes. Globally
pronounceable.

### Existing collisions and assessment

| Entity | Field | Conflict risk |
|--------|-------|---------------|
| Phanes Therapeutics, Inc. | Pharma (TM class 5) | LOW — different class |
| Phanes Networks B.V. / Phanes Cloud | Cloud hosting (class 42 adjacent) | MEDIUM — adjacent class |
| GitHub repos: phanes-o/phanes (microservices), hellofresh/phanes (identity client), martintc/phanes (task manager), ci-ber/PHANES (anomaly detection medical) | Various | LOW — small projects, no commercial TM |
| PhanesNetwork (blockchain Web3 GitHub) | Crypto | LOW — no TM |
| theoi.com / Wikipedia | Mythology reference | NONE — public domain |

**Net assessment:** *medium* risk overall, primarily from Phanes
Networks/Cloud in the cloud-services adjacency. Mitigated by:

- Filing TM as the **compound mark** "PHANES OS" rather than just
  "PHANES" alone.
- Filing in TM class 9 (downloadable software) primarily, with
  class 42 (services) as secondary, naming "operating system kernel"
  as the goods description.
- Phanes Networks transferred operations to SpectraIP in Jan 2025;
  active TM defence may have lapsed.

### Trademark application plan

**Phase 0 month 1:**
- Knockout search complete (this RFC).
- Engage IP attorney for professional search (~$300–500).

**Phase 0 month 2:**
- File TM applications:
  - **USPTO** — classes 9, 42 (~$1500/class).
  - **EUIPO** — classes 9, 42 (~$1200/class).
  - **WIPO Madrid** for international (CN, JP, KR, AU): file later
    once US/EU progress.

**Phase 0 month 3+:**
- TM applications "pending" — gives `™` notation rights.
- Full registration typically 6–12 months. Once registered, `®`
  notation.

**Holder:** Linux Foundation (if incubation accepted) or
PHANES-Project legal entity (if independent).

### Brand assets

| Asset | Phase 0 deliverable |
|-------|---------------------|
| Logo | minimalist mark — to-be-designed by professional in Phase 1 |
| Colour palette | Phase 1 |
| Typography | Phase 1 |
| Wordmark | "PHANES" all-caps in serif |
| Tagline | *"The first mind of the machine."* |

### Domain registration

**Reserve in Phase 0 (recommended set):**

- `phanes.org` — primary
- `phanes.dev` — developer-facing
- `phanes.systems` — alternative
- `phanes.io` — defensive
- `phanes-os.org` — defensive
- `phanesfoundation.org` — defensive
- `phanesos.dev` — defensive

Total cost: ~$200/year for the set. Verify availability via
namecheap / Cloudflare Registrar; reserve all that are free.

### Repository naming

**On GitHub** (and any other forge), repositories are renamed at the
end of Phase 0:

| Current | New |
|---------|-----|
| `robot-os` | `phanes` (kernel + os) — primary repo |
| `robot-brain` | `phanes-brain` |

**Migration steps** (Phase 0 final week):

1. GitHub → repository settings → rename `robot-os` → `phanes`.
   GitHub auto-redirects old URLs. Existing clones keep working.
2. Same for `robot-brain` → `phanes-brain`.
3. Update GitHub org name (if applicable) to `phanes-project` or
   keep current org and put repos under it.
4. Update CI badges, README, links in docs.
5. Update local clones: `git remote set-url origin <new>`.
6. Optional: redirect from old org to new org.

### Internal naming convention going forward

- **The project**: PHANES.
- **The kernel**: "the PHANES kernel" or just "PHANES" in casual
  usage; "PHANES Kernel" in formal docs.
- **The brain server**: "PHANES Brain" (note: this is the Python
  brain server, currently `robot-brain`, to be renamed
  `phanes-brain`).
- **The Foundation/Project entity**: "PHANES Project" (informal),
  "PHANES Foundation" if independent legal entity.
- **Releases**: "PHANES v1.0 (LTS)" / "PHANES 2026.05".

### Avoid in branding

- Lowercase "phanes" alone — looks generic; use ALL-CAPS or
  PascalCase "PHANES".
- "PhanesOS" without space — locks us out of "PHANES" alone if we
  want to apply.
- Abbreviations like "PNS" — meaningless and forgettable.

## Drawbacks

- **Phanes Networks adjacency.** The medium TM risk is real. We
  mitigate via compound mark and class targeting.
- **Pronunciation drift.** English speakers may say "FAH-nez" or
  "FAY-nees"; we've seen "Phaeton" spellings. Mitigated by clear
  pronunciation guide on docs.phanes.org.
- **Obscurity.** "What does Phanes mean?" comes up at every
  conference. We treat this as branding opportunity, not a problem.

## Rationale and alternatives

The full alternative analysis is in the chat history that produced
this RFC. The decision tree:

1. Mythological-classical evokes "first mind/cabeza" perfectly.
2. Three top mythological candidates were Talos, Mimir, Multivac —
   all collided with major existing software/products.
3. PHANES is the deepest-thematic match still available with low
   collision in software.

## Prior art

- **Zephyr** (Greek wind, gentle) — pattern of Greek mythological
  names for OS projects.
- **Athena** (Greek goddess of wisdom) — used by MIT Athena, Oracle
  Athena, etc; saturated.
- **Apollo / Atlas** — taken by Boston Dynamics + many.
- **Talos / Mimir / etc.** — collisions documented above.
- **Tock**, **Hubris**, **Redox** — non-mythological short names
  that worked because of strong project-team identity. We choose a
  mythological route to evoke our positioning ("OS as head/mind").

## Unresolved questions

- **Mark style** — wordmark only ("PHANES"), or wordmark + symbol?
  Working assumption: wordmark only in Phase 0; commission a logo
  in Phase 1.
- **Tagline** finalisation — "The first mind of the machine" is
  proposed; alternatives ("Built in light. For things that must not
  fail.") to be evaluated.
- **TM filing entity.** Decided in RFC-0009.

## Future possibilities

- **PhanesCon** — annual community conference (Phase 3+).
- **PHANES Inside** — partner program for products built on PHANES
  (Phase 4+).
- **PHANES Certified** — products that have passed conformance
  testing (Phase 4+).
