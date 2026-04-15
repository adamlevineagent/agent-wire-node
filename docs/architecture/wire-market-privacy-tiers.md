# Wire Market Privacy Architecture

*How privacy works across all Wire markets (compute, storage, relay). The Wire is pure control plane. All data flows node-to-node through the relay network. Three orthogonal privacy mechanisms make traffic analysis practically impossible at scale.*

---

## Companion documents

- `docs/plans/wire-compute-market-build-plan.md` — compute market implementation
- `docs/plans/storage-market-conversion-plan.md` — storage market conversion
- `docs/plans/relay-market-plan.md` — relay network design
- `GoodNewsEveryone/docs/architecture/wire-compute-market.md` — original vision

---

## The Privacy Problem

When Node A sends a prompt to Node B for inference (or pulls a document from Node B), three concerns arise:

1. **Content exposure**: The provider sees the prompt/document (they must — they process it).
2. **Identity exposure**: The provider knows WHO is asking — tunnel URLs are persistent, requests are correlatable.
3. **Pattern exposure**: Across multiple requests, a provider reconstructs the requester's full research interest, pyramid structure, and intellectual agenda.

---

## The Solution: Three Orthogonal Mechanisms

### 1. Variable Relay Count (Topology Ambiguity)

The requester chooses how many relay hops to use for each job: **0, 1, 2, 5, 12 — any number**. This is a contribution on the requester's dispatch policy (`schema_type: privacy_policy`).

```
Requester → [Relay A → Relay B → ... → Relay N →] Provider
```

A provider receiving a request from a tunnel URL cannot tell if that URL belongs to:
- The requester directly (0 relays)
- The last of 1 relay
- The last of N relays

**The request format is identical regardless of chain length.** No hop count header, no chain metadata, no observable difference. Every tunnel URL on the network is simultaneously a potential requester, relay, or provider — same software, same endpoints.

**Even users who choose 0 relays get plausible deniability.** The provider sees their tunnel URL but can't prove it's a direct connection. It might be the last hop of a 12-relay chain.

### 2. Distributional Opacity (Probabilistic Ambiguity)

**The network NEVER publishes aggregate relay statistics.** No dashboard shows "40% of traffic uses 2 relays." No heartbeat carries relay distribution data. No market surface reveals usage patterns.

If the distribution is unknown, every connection is maximally ambiguous. An attacker can't estimate the probability of any topology because the probability distribution itself is secret.

- The Wire knows aggregate numbers internally for capacity planning but never publishes them
- Individual nodes know only their own relay usage
- Relay nodes know only their own hop traffic
- Even the Wire can't observe the full chain after setup (bytes flow node-to-node)

### 3. Tunnel URL Rotation (Temporal Ambiguity)

Nodes periodically rotate their tunnel URL:
- Node requests a new URL from the tunnel provider
- Pushes new URL to Wire via heartbeat
- Old URL decommissions after in-flight connections drain
- Any correlation built against the old URL is instantly worthless

Rotation frequency is a contribution (`schema_type: privacy_policy`). Combined with variable relay count: the provider sees a DIFFERENT tunnel URL on each request because each relay chain uses different relays, each with rotating URLs.

---

## Wire as Pure Control Plane

The Wire NEVER handles data payloads:

| Channel | What flows | Path |
|---|---|---|
| **Data plane** (relay network) | Prompts, results, document bodies | Requester ↔ Relays ↔ Provider |
| **Control plane** (Wire API) | Matching, settlement, routing instructions, heartbeat | Node ↔ Wire |

The Wire handles: matching bids to asks, charging reservation fees and deposits, returning relay chain routing instructions, settling credits after completion. It never sees a prompt, never sees a result, never sees a document body.

The `wire_compute_jobs` table stores settlement metadata (rates, token counts, costs) but has NO columns for prompt or response content. The Wire is structurally incapable of logging payloads because it never receives them.

### Bootstrap Mode

Before sufficient relay capacity exists, the Wire acts as a relay itself — forwarding payloads between nodes using the same `/v1/relay/forward` endpoint. This is temporary. As relay nodes join, the Wire's relay workload naturally diminishes. The Wire is just another relay during bootstrap, not special infrastructure.

---

## What Each Party Sees

### Provider
- **Sees**: The prompt/document (must — they process it). Model, parameters, job token.
- **Does NOT see**: Requester identity (sees last relay's rotating tunnel URL). Build context, pyramid slug, layer, step name. Any linking metadata.

### Relay Node
- **Sees**: Previous hop tunnel URL, next hop tunnel URL. Encrypted payload (compute) or opaque bytes (storage).
- **Does NOT see**: Requester identity (unless first relay). Provider identity (unless last relay). Payload content (encrypted for compute). Its own position in the chain. Total chain length.

### Wire
- **Sees**: Matching metadata (who matched with whom, at what rate). Settlement data (token counts, latency). Routing instructions (which relays were selected).
- **Does NOT see**: Prompt content. Result content. Document bodies. Never receives any payload.

### Requester
- **Sees**: Their own prompt and result. Cost breakdown. Relay 1's tunnel URL (the entry point to the chain).
- **Does NOT see**: Which provider served them. Relays 2 through N. The provider's tunnel URL. Only the first relay's address is visible.

---

## Privacy Tiers

| Relay Count | Cost | Latency | Privacy Level | What provider can prove |
|---|---|---|---|---|
| **0** | Zero extra | Zero extra | Plausible deniability | Nothing — could be direct or relayed |
| **1** | 1 hop fee | +50ms | Provider doesn't know requester | Nothing — could be 1 of 1 or 1 of N |
| **2** | 2 hop fees | +100ms | No single non-provider knows both endpoints | Nothing |
| **N** | N hop fees | +N×50ms | N-way collusion required | Nothing |

### Additional Privacy Controls

- **Fan-out policy**: `max_jobs_per_provider` (contribution) limits how many calls any single provider sees per build. Setting to 1 = each provider sees exactly one isolated prompt.
- **E2E encryption**: For compute payloads, requester encrypts with provider's ephemeral public key (per-job, generated by Wire). Relays see only ciphertext.
- **No encryption for storage**: Document bodies are public content. Privacy is about hiding WHO pulled, not WHAT was pulled.

---

## Future Privacy Tiers (Stubbed)

### Clean Room

> **Status: Architectural stub. Not in initial market launch.**

An ephemeral Docker container on the provider's machine. Encrypted I/O — provider never sees plaintext. The container loads, processes, returns encrypted result, and implodes.

**Open questions:** GPU passthrough (Linux/NVIDIA only?), model loading overhead, key security (Docker is not a hardware security boundary), attestation protocol.

### Vault / SCIF

> **Status: Architectural stub. Requires Wire-owned or Wire-audited hardware.**

Inference on Wire-owned hardware (Mac Studios with 512GB). Zero trust chain beyond the Wire. Enterprise clients can audit hardware. Near-frontier model quality.

**Open questions:** Hardware deployment, audit certification, TEE integration (SGX, AMD SEV, ARM CCA for cryptographic attestation).

### Split Inference

> **Status: Research concept. Not scheduled.**

Prompt split across multiple nodes so no single node sees the full input. Enabled by chunked architecture. Limited to structured/decomposable prompts. Quality degradation from fragmented context.

---

## The Mature Network

At scale, the Wire network looks like this to an outside observer:

```
[cloud of rotating tunnel URLs]
     ↕ encrypted streams ↕
[cloud of rotating tunnel URLs]
```

- Can't tell who's a requester, relay, or provider (same software, same endpoints)
- Can't tell how many hops any connection traverses (0 to N, unknown distribution)
- Can't correlate connections over time (tunnel URLs rotate)
- Can't observe the routing topology (Wire sets it up, data flows node-to-node)
- Can't prove any two connections involve the same node
- Can't determine whether a direct connection is actually direct

The Wire itself can't observe the data flow after setup. It issues routing instructions and the bytes flow between nodes. The Wire sees: "I told N relays to form a chain. Settlement says it completed." It never saw the payload.
