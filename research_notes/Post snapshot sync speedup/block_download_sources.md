# Block download, networking and other block sources for Bitcoin Core v31.1 after an assumeutxo snapshot (935,000 → ~974,500)

Context checked on 2026-11-04. The live node `allod2-bitcoind-1` runs `/Satoshi:31.1.0/`. It has `networkactive: false` because the snapshot load is in progress, 0 connections, `headers: 974509`, an old chainstate at height 269,636, and `pruned: true` with a `prune_target_size` of 10,000 MiB. The generated `bitcoin.conf` contains `prune=10000`, `dbcache=2048`, `blocksonly=1`, `blockfilterindex=0` and `txindex=0`. Its log shows `[snapshot] loaded 164241311 ... coins from snapshot 0000…0fb5ee` at 09:19:08Z. Local services are `WITNESS, NETWORK_LIMITED, P2P_V2`. In compose (`deploy/compose.yaml`), the bitcoind service has `mem_limit: ${BITCOIN_MEMORY_LIMIT:-2g}`. `images/bitcoind/assumeutxo.sh` calls `setnetworkactive false` once the base header is known, and turns networking back on after `loadtxoutset`. Source code below was fetched from `raw.githubusercontent.com/bitcoin/bitcoin/v31.1/…` (marked "v31.1 src").

## 1. How block download works in v31.1 (window, in-flight limit, stalling, peers), and can 30 MB/s be saturated?

### Takeaway
Block download is parallel across outbound peers. Each peer can have at most 16 blocks in flight, and the node only fetches up to 1,024 blocks beyond the last block it has in common with that peer. By default there are 8 full-relay and 2 block-relay-only automatic outbounds, plus up to 8 addnode connections. Raw bandwidth is rarely what limits IBD. Core developers say validation is the bottleneck when a sync takes long. Downloading 65–70 GB at 30 MB/s takes only about 37–40 minutes, so on Allod hardware validation (CPU, disk, dbcache) will very likely set the pace, not the network.

### Cited Findings
- `MAX_BLOCKS_IN_TRANSIT_PER_PEER = 16`, `BLOCK_DOWNLOAD_WINDOW = 1024`, `BLOCK_STALLING_TIMEOUT_DEFAULT{2s}`, `BLOCK_STALLING_TIMEOUT_MAX{64s}`, `BLOCK_DOWNLOAD_TIMEOUT_BASE = 1` (×10 min block interval), `BLOCK_DOWNLOAD_TIMEOUT_PER_PEER = 0.5` (+5 min per other downloading peer) — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- The source comment on the window says: "Larger windows tolerate larger download speed differences between peer, but increase the potential degree of disordering of blocks on disk (which make reindexing and pruning harder)". The code applies it as `nWindowEnd = pindexLastCommonBlock->nHeight + BLOCK_DOWNLOAD_WINDOW` — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- Stalling: when a peer holds the block that blocks the window from moving, and no other peer can be given work, it is marked as the staller. It is disconnected after `m_block_stalling_timeout` (initially 2 s). After each stall disconnect the timeout doubles, up to 64 s, so that a slow local link does not cause several peers to be dropped. Each connected block brings it back toward 2 s (×0.85). The stalling disconnect code does **not** check NoBan or any other permission — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp); the adaptive timeout arrived in v25 via [PR #25880](https://mirror.b10c.me/bitcoin-bitcoin/25880/)
- One maintainer described the rule like this: a peer is treated as stalling when its bandwidth is below about 1/64 of the total, because 1024/16 = 64 — [summary of #29664 / #25880 discussion](https://github.com/bitcoin/bitcoin/pull/29664)
- Pieter Wuille argued against making the window bigger for most of IBD, because "1024 blocks is way too much; that value was set at a time when blocks were tiny" — [PR #29664 discussion](https://github.com/bitcoin/bitcoin/pull/29664)
- [PR #32180](https://mirror.b10c.me/bitcoin-bitcoin/32180/) was merged on 2025-11-10. It fixed `pindexLastCommonBlock` lagging behind, which had caused blocks not to be requested and peers to be wrongly flagged as stallers. It also removed special assumeutxo logic, so after a snapshot load `pindexLastCommonBlock` moves to the snapshot height on its own. The fix is in v31.1: `fork_point = LastCommonAncestor(state->pindexBestKnownBlock, m_chainman.ActiveTip())` is at net_processing.cpp line ~1427 — [PR #32180](https://mirror.b10c.me/bitcoin-bitcoin/32180/); [v31.1 src](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- During IBD, blocks are only requested from peers with `sync_blocks_and_headers_from_peer && !IsLimitedPeer(peer)`. That flag is set when `fPreferredDownload` is true, which needs an outbound connection, or an inbound one with NoBan, that is not addr-fetch and can serve blocks. Inbound peers are used only when there are no preferred peers or nothing is in flight. Pruned (`NODE_NETWORK_LIMITED`) peers are never used for IBD block download — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- Headers are requested from a single peer only, unless the best header is less than 24 h old — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- `MAX_OUTBOUND_FULL_RELAY_CONNECTIONS = 8`, `MAX_BLOCK_RELAY_ONLY_CONNECTIONS = 2`, `MAX_ADDNODE_CONNECTIONS = 8`, `MAX_FEELER_CONNECTIONS = 1`, `DEFAULT_MAX_PEER_CONNECTIONS = 125`. Automatic outbound slots are `min(8, max_automatic)` plus `min(2, rest)`, so `-maxconnections` above 125 does **not** add outbound (download) peers. Lowering it below about 11 removes some — [v31.1 src net.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net.h)
- sipa (2025-12-19): "If synchronization takes days, it's not the downloading that's the bottleneck, but validation." Other reported bottlenecks were storage latency on network storage (andrewtoth) and Tor, which "can take days" (l0rinc). l0rinc claimed v30 IBD is about 200% faster than v25 and reported 7–10 h validation on Umbrel and Raspberry Pi 5 class devices — [bitcoin/bitcoin#34121](https://mirror.b10c.me/bitcoin-bitcoin/34121/)
- Lopp's 2025 test: Core 30.0, `dbcache=24000`, `assumevalid=0`, `connect=<LAN node>`. It synced to block 928,000 in 12 h 7 min and downloaded 690 GB. That averages about 15.8 MB/s from a single LAN peer, on a machine that is CPU/validation-bound with full script checks. The only older peak figure is "80 MB/s" for Core 0.15.1 in 2018 — [Lopp 2025 node performance tests](https://blog.lopp.net/2025-bitcoin-node-performance-tests/)
- Parallel input prefetch ([PR #31132](https://mirror.b10c.me/bitcoin-bitcoin/31132/), now continued in #35295). Reported speedups vary a lot: about 40% for blocks 840k–850k with 15 threads (Nov 2024), about 14% at default dbcache and about 6% at dbcache=5000 (Sep 2025), and slightly slower on an HDD with dbcache=10000. The v31.1 source has a `CoinsViewOverlay` (`validation.cpp` ~1864), but I did not confirm whether the threaded fetcher shipped in v31 — [PR #31132](https://mirror.b10c.me/bitcoin-bitcoin/31132/)

### Inferences
- In-flight ceiling: 10 automatic outbounds × 16 blocks ≈ 160 blocks in flight. At about 1.7 MB per recent block that is about 270 MB requested at once. Bandwidth-delay product is therefore not what limits a 30 MB/s link. A few healthy peers can fill it, and in practice the 1,024-block window and validation speed decide the pace.
- Pure transfer time for 39.5k blocks / 65–70 GB at 30 MB/s is about 36–39 min. At about 15 MB/s (Lopp's single-LAN-peer, CPU-bound average) it is about 75 min. On a typical Allod box (mini-PC or Pi 5, 2 GB memory limit, dbcache 2048, pruned), validating about 36k recent blocks with full script checks (see §2: assumevalid ends at 938,343) should take several hours. So the network will not be saturated, and adding peers or bandwidth gains little once validation is the bottleneck.
- More peers help only when individual peers are slow, because more parallel 16-block slots spread across peers. The default of 8 full-relay + 2 block-relay-only is usually enough. `-maxconnections` cannot raise the outbound count.
- **Risk to check:** `dbcache=2048` together with `mem_limit: 2g` leaves no room for the rest of bitcoind's memory (mempool, buffers, block reads). The log already shows "Cache size (2116214544) exceeds total space (2115627909)" during the snapshot load. The OOM risk during the post-snapshot sync, while a background chainstate also exists, should be looked at. Unvalidated.

### Gaps
- I found no recent (2025–2026) published measurement of IBD download throughput from random clearnet peers in MB/s. #34121 and Lopp's tests give no per-peer figures.
- No benchmark of v31.1 validation speed for blocks 935k–975k on Pi 5 or mini-PC class hardware was found.

## 2. Assumeutxo: how downloads are split between the snapshot chainstate and the background chainstate

### Takeaway
In v31.1 each peer's 16-slot budget goes first to the snapshot (active, to-tip) chainstate through `FindNextBlocksToDownload`. Any slots left over go to the historical range (`TryDownloadingHistoricalBlocks`, up to 1,024 blocks ahead of the background tip). So the background chainstate does take bandwidth, CPU and disk whenever the to-tip window is full or blocked. There is no config option to defer it. On a pruned node, the snapshot-era blocks are kept until the background validation reaches the base.

### Cited Findings
- The source comment says: "If there are multiple chainstates, download blocks for the current chainstate first, to prioritize getting to network tip before downloading historical blocks." `FindNextBlocksToDownload(peer, get_inflight_budget(), …)` runs first, then `TryDownloadingHistoricalBlocks(peer, get_inflight_budget(), vToDownload, from_tip, historical_blocks->second)` gets whatever budget is left. `get_inflight_budget()` is `16 - vBlocksInFlight.size()` — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- Historical downloads are skipped for limited (pruned) peers and for peers whose best known block does not have the snapshot base as an ancestor. Their window is `min(from_tip + 1024, snapshot_base_height)` — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- Pruning with a snapshot: "Only prune blocks _after_ the snapshot if this is a snapshot chain" (`prune_start = SnapshotBase()->nHeight + 1`) until the background chainstate is validated — [v31.1 src validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- v31.1 mainnet assumeutxo heights are 840,000, 880,000, 910,000 and 935,000 (base hash `0000…0147034958af1652b2b91bba607beacc5e72a56f0fb5ee`, which matches the live log). `defaultAssumeValid` is block 938,343 (`0000…0ccebd6d…ba5ac`) — [v31.1 src chainparams.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/kernel/chainparams.cpp)
- `-reindex` and `-reindex-chainstate` wipe a loaded snapshot chainstate as well ("The snapshot can then be reloaded via RPC") — [v31.1 src init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- Sjors and willcl-ark: AssumeUTXO "doesn't reduce total sync time, but gets a node to tip quickly", and it "is still bottlenecked on validation speed" — [bitcoin/bitcoin#34121](https://mirror.b10c.me/bitcoin-bitcoin/34121/)

### Inferences
- In the steady state before reaching the tip, the to-tip chain needs validation to keep up. When validation is slower than download, the 1,024-block to-tip window fills up and the spare slots start pulling historical blocks (on this node, from 269,636 up toward 935,000, which is hundreds of GB). Those blocks then compete for CPU (`ActivateBestChain` on the background chainstate), dbcache and disk I/O. Expect the to-tip phase to be somewhat slower than a node without a background chainstate, by an unmeasured amount.
- Script verification is skipped only for blocks up to 938,343 (assumevalid), which is about 3.3k of the 39.5k blocks. The other ~36k blocks get full script checks, so this phase is CPU-heavy.
- There is no supported knob to defer background download. Workarounds would be unsupported: for example `setnetworkactive` toggling or temporarily connecting only to pruned peers, which serve the to-tip blocks within 288 of the tip but not deep history. Neither fits a 39k-block catch-up. Treat this as not achievable through config.
- Allod's existing pause logic (`setnetworkactive false` during the snapshot download) is consistent with this. After the load, both chainstates become active at once.

### Gaps
- No PR or flag found (as of v31.1) to throttle or defer background chainstate validation. I did not search the PR list for proposals.
- The size of the bandwidth or CPU "steal" in practice was not measured or found in sources.

## 3. -blocksonly, -maxuploadtarget, -listen=0, Tor/onlynet, BIP324 v2 transport

### Takeaway
`-blocksonly` (already set in Allod) saves transaction-relay bandwidth and CPU, a small but real gain. `-maxuploadtarget` and `-listen=0` limit what the node serves to others and do not limit its own downloads directly. Tor/onlynet=onion slows IBD sharply ("can take days"). v2 transport (BIP324) is on by default and adds negligible overhead.

### Cited Findings
- `-blocksonly`: "Whether to reject transactions from network peers… RPC transactions are not affected". In blocksonly mode `whitelistrelay` is set to 0 and maxmempool is reduced — [v31.1 src init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- `-maxuploadtarget`: "Tries to keep outbound traffic under the given target per 24h. Limit does not apply to peers with 'download' permission or blocks created within past week." — [v31.1 src init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- l0rinc: on Tor, IBD "can take days"; 500 Mbit clearnet "should suffice" — [bitcoin/bitcoin#34121](https://mirror.b10c.me/bitcoin-bitcoin/34121/)
- The live node already advertises `P2P_V2` (BIP324) — `getnetworkinfo` on `allod2-bitcoind-1`

### Inferences
- `-listen=0` only stops inbound connections. Inbound peers are not preferred download sources during IBD, so this has little effect on download speed, though it saves upload bandwidth on a ~30 MB/s symmetric-or-not link.
- BIP324 v2 adds about 20 bytes of overhead per message plus ChaCha20 encryption. Per MB-scale block, the CPU and byte cost is negligible compared with validation. I found no source with a number for this.
- Keep clearnet (ipv4/ipv6) enabled for IBD. The node currently has onion `reachable: false`, which is good for sync speed.

### Gaps
- No measured numbers for the blocksonly gain during IBD; no measured BIP324 overhead numbers found.

## 4. Using a known fast peer (-addnode/-connect, whitelist permissions)

### Takeaway
`-addnode=<VPS/LAN node>` adds an outbound, preferred-download connection on top of the automatic 10. `-connect` makes that node the only peer. On the serving node, `whitelist=download@<allod-ip>` exempts Allod from that node's `-maxuploadtarget`. On the Allod side, `noban` only matters for inbound peers (it makes them preferred) and for headers-timeout tolerance. It does **not** exempt a peer from the stalling disconnect. Lopp's single-LAN-peer method averaged about 15.8 MB/s while CPU-bound, which shows that one good peer can carry IBD.

### Cited Findings
- `fPreferredDownload = (!IsInboundConn() || HasPermission(NoBan)) && !IsAddrFetchConn() && CanServeBlocks(peer)` — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- A NoBan peer is not disconnected on headers-sync timeout ("Timeout downloading headers from noban peer, not disconnecting"). The block stalling disconnect has no permission check — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- `MAX_ADDNODE_CONNECTIONS = 8`, separate from the automatic outbound slots — [v31.1 src net.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net.h)
- `download` permission bypasses `-maxuploadtarget` on the serving node — [v31.1 src init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- Lopp used `connect=<local node IP>` "to keep the bandwidth of peers on the public network" from being a bottleneck: 690 GB in 12 h 7 min (Core 30.0, assumevalid=0) — [Lopp 2025](https://blog.lopp.net/2025-bitcoin-node-performance-tests/)

### Inferences
- A fast peer must be an archival (non-pruned) node for historical blocks, and at least NODE_NETWORK for IBD at all, because limited peers are skipped during IBD.
- With `-connect` to a single peer, any stall of that peer stalls the whole sync and the stall logic has no one to switch to. `-addnode` (which keeps random peers) is the safer choice.
- Expected gain: large only when random peers are slow (Tor, peers far away, congested networks). On a 30 MB/s link with validation as the bottleneck, probably modest. There is no source-backed figure for "how much faster". This is an inference.
- The trust model does not change. Blocks from any peer are fully validated, and a malicious addnode peer can only withhold or slow blocks.

### Gaps
- No public A/B measurement of addnode-fast-peer vs random-peers for a recent v30/v31 IBD.

## 5. Alternative block sources: -loadblock, copying blocks/, archives/torrents, -reindex; shipping blocks on the USB image

### Takeaway
`-loadblock=<file>` still exists in v31.1. It imports raw serialized blocks (the bootstrap.dat / blk*.dat format) at startup in the `initload` thread, through `LoadExternalBlockFile` → `AcceptBlock`. Blocks then go through normal validation, so the source does not need to be trusted for validity, only for availability. Core maintainers abandoned bootstrap.dat because syncing from the network downloads and validates in parallel. A blocks file must be complete before import, and validation is the bottleneck anyway. Copying `blocks/` + `chainstate/` from another node is fast but skips validation (trusted). For Allod, shipping 935k→release-height blocks on the USB image only saves download time, which is the cheap part (about 40 min at 30 MB/s). It would add about 7 GB per month of chain to the image. Low value.

### Cited Findings
- `-loadblock=<file>`: "Imports blocks from external file on startup". Files are imported in the `initload` background thread before `ActivateBestChain` — [v31.1 src init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- `LoadExternalBlockFile` detects out-of-order blocks ("Out of order block %s, parent %s not known"), queues them in `blocks_with_unknown_parent`, and calls `AcceptBlock(pblock, state, nullptr, true, dbp, nullptr, true)`. This is the same acceptance path as network blocks, and full connect-time validation happens in `ActivateBestChain` — [v31.1 src validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- While blocks are loading, the node does not start headers sync (`!m_chainman.m_blockman.LoadingBlocks()` gate) — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- Auto-import of `bootstrap.dat` was removed (#15954 / #17044). The file must now be passed with `-loadblock` — [PR #17044](https://mirror.b10c.me/bitcoin-bitcoin/17044/)
- sipa (2025-12-19): bootstrap "was abandoned because it's generally faster to just sync from the network since Bitcoin Core 0.12… If you use bootstrap.dat, you need to first download the whole file, and only then validate it." Importing it "still validated the whole history"; copying a chainstate skips validation (trust). Issue closed by willcl-ark 2026-01-13, with "no plans to reintroduce bootstrap" — [bitcoin/bitcoin#34121](https://mirror.b10c.me/bitcoin-bitcoin/34121/)
- Reported practice: copying `blocks/` + `chainstate/` from a friend's node took about 7 h to download and "saved at least two days" (trusted copy). chainstate.io offers pruned blocks and chainstate (v28–30.2, daily), which is trusted as well — [bitcoin/bitcoin#34121](https://mirror.b10c.me/bitcoin-bitcoin/34121/)
- `-reindex` rebuilds the block index and chainstate from local blk files and wipes a loaded snapshot chainstate — [v31.1 src init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- Snapshots are hosted at utxo.download and via torrent (Sjors, willcl-ark) — [bitcoin/bitcoin#34121](https://mirror.b10c.me/bitcoin-bitcoin/34121/)

### Inferences
- **Trust model:** `-loadblock` keeps full validation. Validity cannot be faked, but a bad file wastes time. Copying `blocks/` + `chainstate/` is a trusted shortcut, not acceptable as an Allod default under its "never trust" posture. Copying only `blocks/` and running `-reindex` re-validates everything, but wipes the snapshot and needs the whole history. Not suitable.
- **With assumeutxo (unverified, needs a test):** after `loadtxoutset`, a restart with `-loadblock=blocks_935001_X.dat` should accept these blocks because their headers are known. They would then connect to the snapshot chainstate. It is unclear whether blocks above the snapshot base are connected to the snapshot chainstate during import, and how this interacts with the background chainstate's `ActivateBestChain`. Test on regtest/signet before relying on it.
- **Pruning:** imported blocks are written into new blk files and pruned normally afterwards, but post-snapshot blocks are kept until background validation finishes (§2). So on a pruned Allod node, all 935k→tip blocks (~65–70 GB) stay on disk during the sync, alongside historical blocks within the prune budget. This matters more for disk sizing than any shipping scheme. Check against `images/bitcoind/prune-size.sh`.
- **Shipping blocks on USB/mirror:** recent blocks run about 1.6–1.8 MB each × ~4,300 blocks/month ≈ 7 GB per month past height 935,000. The image already holds a 9.4 GB snapshot (ISO about 9.6 GB, 16 GB stick). Shipping, say, 3 months of blocks would add about 20 GB and need a 32 GB+ stick. It would save about 11–12 min of download per 20 GB at 30 MB/s, with the same validation cost. The data goes stale as releases age, so it is not practical. An HTTP mirror plus `-loadblock` is equally unattractive: download must finish before import, which loses the download/validation overlap of P2P.
- A better lever is the snapshot height itself. The release only accepts heights hardcoded in chainparams (935k is the newest in v31.1), so a closer-to-tip snapshot needs a newer Core release.

### Gaps
- No primary source found for current HTTP/torrent mirrors of raw blk files for recent heights, beyond chainstate.io (trusted, pruned) and utxo.download (snapshots).
- I did not test `-loadblock` together with an active assumeutxo snapshot chainstate.

## 6. Compact blocks (BIP152), v29–v31 networking changes, and upcoming work (SwiftSync etc.)

### Takeaway
Compact blocks do not help during IBD. Old blocks are served whole (`MAX_CMPCTBLOCK_DEPTH = 5`), and a blocksonly node has no mempool to rebuild from. v31.0 (2026-04-19) had no IBD-download release notes beyond raising the default `-dbcache` to 1024 MiB. The stalling-window fix #32180 (merged 2025-11-10) is in v31.1. SwiftSync is not in any release: the Core PR #34004 was closed unmerged on 2026-05-31, and the BIP draft (#2152) was opened 2026-05-06. Even if it were available, it targets pruned full IBD and would conflict with assumeutxo.

### Cited Findings
- `MAX_CMPCTBLOCK_DEPTH = 5`: "For older blocks, a regular BLOCK response will be sent." — [v31.1 src net_processing.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/net_processing.cpp)
- PR #29664 discussion: near the tip (≤1024 blocks) the stall rule cannot fire, and "We can't do compact blocks yet". Proposed fix: add another peer for the critical block after about 30 s — [PR #29664](https://github.com/bitcoin/bitcoin/pull/29664) (merge status not verified)
- v31.0 release notes: default `-dbcache` raised from 450 to 1024 MiB (#34692); new `/rest/blockpart/` endpoint (#33657); asmap embedded but off by default (#34696); `startingheight` removed from getpeerinfo (#34197). No notes on stalling, assumeutxo, pruning or loadblock. Released 2026-04-19 — [Bitcoin Core 31.0 release notes](https://bitcoincore.org/en/releases/31.0/)
- #32180 merged 2025-11-10 (stalling false positives, window rebasing after stalls; removes assumeutxo-specific `pindexLastCommonBlock` logic) — [PR #32180](https://mirror.b10c.me/bitcoin-bitcoin/32180/)
- SwiftSync: an April 2025 PoC reported about a 5.28× IBD speedup over default (assumevalid, no assumeutxo) — [Optech 2025-04-11](https://bitcoinops.org/en/newsletters/2025/04/11/); [Delving thread](https://delvingbitcoin.org/t/swiftsync-speeding-up-ibd-with-pre-generated-hints-poc/1562)
- Core PR #34004 (rustaceanrob, opened 2025-12-04) was closed without merging on 2026-05-31, with the work moved to the BIP process. It covers pruned nodes only; parallel out-of-order download is "future iteration". The proposal says to fail at startup if combined with assumeutxo. The hints file is about 173 MB run-length encoded up to height 928k. No speedup was measured in the PR — [PR #34004](https://mirror.b10c.me/bitcoin-bitcoin/34004/)
- BIP draft "SwiftSync Specification" #2152, opened 2026-05-06 — [bips#2152](https://mirror.b10c.me/bitcoin-bips/2152/)

### Inferences
- For Allod on v31.1 in Nov 2026, nothing upcoming (SwiftSync, "assumeblocks", block archive services) is usable. The practical levers are config (dbcache within the memory limit, `blocksonly=1`, clearnet, optional `-addnode` to a trusted fast archival node, `-par`) and hardware (CPU and SSD for the ~36k fully script-validated blocks).

### Gaps
- I did not confirm whether #29664 (parallel download near tip) or #35295 (parallel input fetch) are merged or in v31.1.
- I found no concrete "assumeblocks" proposal or block-archive service (trusted-hash block files) with a status as of late 2026.
