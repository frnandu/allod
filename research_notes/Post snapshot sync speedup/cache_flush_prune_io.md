# Bitcoin Core v31.1: UTXO cache, flushing, pruning, LevelDB and I/O effects on assumeutxo snapshot→tip sync (935,000 → ~974,500)

Source code facts come from the `v31.1` tag, read directly:
[validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp), [validation.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.h), [coins.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/coins.cpp), [node/blockstorage.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.cpp), [node/blockstorage.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.h), [node/caches.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/caches.cpp), [kernel/caches.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/kernel/caches.h), [txdb.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/txdb.cpp), [dbwrapper.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/dbwrapper.cpp), [dbwrapper.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/dbwrapper.h), [init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp), [node/chainstate.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/chainstate.cpp).
"Live" facts come from read-only commands against container `allod2-bitcoind-1` on 2026-11-04 (v31.1.0; `debug.log`, `getchainstates`, cgroup files). I cite these as [Live node](#).

## Live observations on this machine (context for every question)

### Takeaway
The live config differs from the brief: `blockfilterindex=0`, not 1. The coins cache is 2038 MiB, and the snapshot chainstate gets 1936 MiB (95%) of it. The load took 10m41s, and 11 intermediate flushes took 252 s (39%). The sync then started with an empty cache at about 0.85–1.2 s per block. The node is also close to its 6 GiB container limit.

### Cited Findings
- `bitcoind -version` reports **v31.1.0**. The generated `/data/bitcoin.conf` contains `prune=10000`, `dbcache=2048`, `blocksonly=1`, **`blockfilterindex=0`** and `txindex=0`. The brief says blockfilterindex=1; the live file does not. — [Live node](#)
- The startup "Cache configuration" log shows 2.0 MiB for the block index DB, 8.0 MiB for the chainstate DB (LevelDB) and **2038.0 MiB for the in-memory UTXO set, "plus up to 4.8 MiB of unused mempool space"**. The tiny mempool bonus exists because `-blocksonly=1` soft-sets `-maxmempool` to `DEFAULT_BLOCKSONLY_MAX_MEMPOOL_SIZE_MB` (init.cpp lines 826–827). — [Live node](#); [init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- **Before the snapshot load**, the original chainstate was at height 269,636. This chainstate becomes the background ("historical") chainstate. While the snapshot loaded, its cache was shrunk to 20.4 MiB coinstip and 0.1 MiB coinsdb. — [Live node](#)
- **Load timeline:** `loading 164241311 coins` started at 09:08:27. `loaded … (268 MB)` came at 09:19:08, so the coin load took **10m41s (641 s)**. "validated snapshot" and "successfully activated" came at 09:21:56, after about 2m48s of hashing and the final save. — [Live node](#)
- **Intermediate flushes during the load:** 11 runs of `FlushSnapshotToDisk: flushing coins cache (2116 MB)`, each with the warning "Flushing large (14760000 entries) UTXO set". They took 23.6, 18.4, 30.3, 10.6, 30.6, 36.4, 8.0, 26.6, 8.9, 37.3 and 21.6 s, **252 s in total, or about 39% of the 641 s coin-load phase**. A flush came after every ~14.76M coins, which works out to **~143 bytes of cache memory per coin** (2116 MB / 14.76M). — [Live node](#)
- After activation, the snapshot chainstate got **coinstip 1936.1 MiB and coinsdb 7.6 MiB**, which is 95% of each. — [Live node](#)
- On disk after the load: `/data/chainstate_snapshot` is **11 GB**, `/data/chainstate` (background) is 425 MB and `/data/blocks` is 3.9 GB (already pruned). — [Live node](#)
- **The cache starts empty after activation.** At height 935,026 it was 29.7 MiB (220,794 txo). At 935,100 it was 80.7 MiB (612,353 txo), and at 935,113 it was 88.0 MiB (675,054 txo). That is roughly **0.67 MiB and ~5.2k txo of net growth per block**. The early sync rate was about 150 blocks in about 3 min (09:21:56 → 09:24:53 at height 935,151), or **≈0.85–1.2 s per block**. — [Live node](#)
- **Memory:** cgroup `memory.max` = 6442450944 (6 GiB). `memory.current` was 6.21 GB during the load. Afterwards bitcoind showed VmHWM 6,177,684 kB, VmRSS 5,205,584 kB, cgroup `anon` 3.45 GB and `file` (page cache) 2.89 GB. — [Live node](#)
- **Storage:** `/data` is a bind mount of `/dev/nvme0n1p1`, which is **ext4 with `rw,relatime`** and 203 GB free of 836 GB. The NVMe I/O scheduler is `[none]`. — [Live node](#)
- **Allod tuning script:** `sync_dbcache` returns min(headroom/2, 2048) MiB. The script's `mem_mib` derives cgroup headroom as `memory.max − memory.current − 768 MiB`. — `/home/fmar/workspace/allod/images/bitcoind/sync-tuning.sh`. In `deploy/compose.yaml`, `mem_limit`/`memswap_limit` default to `${BITCOIN_MEMORY_LIMIT:-2g}` and `BITCOIN_DBCACHE` defaults to 450. — `/home/fmar/workspace/allod/deploy/compose.yaml`

### Inferences
- **The 6 GiB cgroup limit also caps the OS page cache.** Only ~2.9 GB of page cache was resident for an 11 GB snapshot LevelDB plus block files. Right after activation the coins cache is cold, so most input lookups during the first thousands of blocks go to LevelDB/disk. These lookups are serial in v31.1, because no parallel input fetching shipped (see Q1). The observed ~1 s/block, which is far below the ~30 MB/s network capacity (~17 blocks/s), fits I/O- or latency-bound lookups, but this hypothesis was not profiled.
- `memory.current` counts page cache, so `mem_mib` in sync-tuning.sh **underestimates real headroom** when the cgroup has a lot of cached file data, and the 2048 MiB cap binds anyway.
- Anonymous memory was ≈ dbcache + ~1.4 GB at dbcache=2048 (3.45 GB anon). That overhead covers the background chainstate cache, LevelDB memtables, block buffers, the script-check queue and allocator slack. It is a single sample, not a law.

### Gaps
- No profiling (perf/iostat) was done to prove whether the post-activation sync is lookup-bound or download-bound.
- The assumevalid height for v31.1 was not checked. The hash `00000000000000000000ccebd6d74d9194d8dcdc1d177c478e094bfad51ba5ac` is in the log. Blocks above that height need full script verification, which shifts the bottleneck toward CPU.

## Q1: How does dbcache size affect IBD speed in Core 28–31, when does the coins cache flush, and what changed in 28/29/30/31?

### Takeaway
In v31.1 the coins cache is **wiped** only when it is ≥90% full (on a periodic check), when it is over its limit, or on a forced flush. Prune events and the randomized 50–70 min periodic write use a non-wiping **`Sync()`**. Benchmarks show large gains from 450 → 2048 MiB (1.18–2.46×) and small gains from 2 → 3 GiB (~2–14%). Larger caches mostly pay off by avoiding wipes.

### Cited Findings
- **Flush logic in v31.1 `Chainstate::FlushStateToDisk`:**
  - `fCacheLarge` is true when mode is `PERIODIC` and the cache state is ≥ `LARGE`.
  - `fCacheCritical` is true when mode is `IF_NEEDED` and the cache state is ≥ `CRITICAL`.
  - `fPeriodicWrite` is true when mode is `PERIODIC` and `now >= m_next_write`.
  - `fFlushForPrune` is true when the prune check selected files.
  - `empty_cache = FORCE_FLUSH || fCacheLarge || fCacheCritical`, and the write is `empty_cache ? CoinsTip().Flush() : CoinsTip().Sync();`.
  - **Prune events and periodic writes therefore keep the cache warm. Only a full cache, or a forced flush, empties it.** — [validation.cpp v31.1](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **Thresholds:** CRITICAL means `cacheSize > nTotalSpace`, where nTotalSpace = coins cache budget + max(0, maxmempool − mempool usage). LARGE means above `max(90% of total, total − 10 MiB)`. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp), [validation.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.h)
- **Periodic write interval:** `DATABASE_WRITE_INTERVAL_MIN{50min}` and `DATABASE_WRITE_INTERVAL_MAX{70min}`, with the next write drawn uniformly at random in that window. There is no separate 24 h `DATABASE_FLUSH_INTERVAL` any more. — [validation.cpp v31.1](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp). This comes from PR #30611, "write chainstate to disk every hour". Its thread reports full IBD effects that range from no measurable change to "~8%"/"10% slower" on HDD at 20–30 GB dbcache, and "close" results at 2 GB dbcache on SSD (30685.8 s before vs 31140.0 s with 50–70 min). The PR's earlier description notes that the old 24 h periodic flush "does not clear the dbcache" since #28233. — [PR #30611 mirror](https://mirror.b10c.me/bitcoin-bitcoin/30611/). The mirror page lists the PR as open, but the v31.1 source contains the 50–70 min constants.
- **`Sync()` vs `Flush()`:** `Flush()` batch-writes the dirty entries with `will_erase=true` and then calls `cacheCoins.clear()`. `Sync()` writes the dirty entries with `will_erase=false` and keeps the clean ones. — [coins.cpp v31.1](https://github.com/bitcoin/bitcoin/blob/v31.1/src/coins.cpp)
- **v28.0, PR #28280:** "The chainstate database flushes that occur when blocks are pruned will no longer empty the database cache." — [v28.0 release notes](https://raw.githubusercontent.com/bitcoin/bitcoin/master/doc/release-notes/release-notes-28.0.md). The PR adds a doubly linked list of dirty entries so that Sync only visits dirty entries. Benchmarks are full IBD to 800k; the data dates from 2023, which is pre-#20827:
  - prune=550, dbcache=16384: 8:52:57 → **6:01:00 (32%)**
  - prune=10000, dbcache=5000: 8:19:59 → **5:56:39 (28.8%)**
  - prune=0: ~2.6–2.7% change
  - — [PR #28280](https://github.com/bitcoin/bitcoin/pull/28280)
- **v29.0, PR #28358:** the maximum `-dbcache` was dropped; values above 16 GiB used to be silently reduced. — [v29.0 release notes](https://raw.githubusercontent.com/bitcoin/bitcoin/master/doc/release-notes/release-notes-29.0.md). The v31.1 `CalculateDbCacheBytes` has no 64-bit cap; only 32-bit systems are capped at 1 GiB. — [node/caches.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/caches.cpp)
- **v30.0:** the only cache-related item is that dbcache is capped at 1 GiB on 32-bit systems (#32530). — [v30.0 release notes](https://raw.githubusercontent.com/bitcoin/bitcoin/master/doc/release-notes/release-notes-30.0.md)
- **v31.0, PR #34692:** the default `-dbcache` rose from 450 to **1024 MiB** when ≥4096 MiB of RAM is detected. The notes warn that "the detected RAM may exceed the memory actually available, which can lead to out-of-memory conditions" in containers and similar setups. — [v31.0 release notes](https://raw.githubusercontent.com/bitcoin/bitcoin/master/doc/release-notes/release-notes-31.0.md); [PR #34692](https://github.com/bitcoin/bitcoin/pull/34692). Code: `HIGH_DEFAULT_DBCACHE{1024_MiB}`, `HIGH_DEFAULT_DBCACHE_MIN_TOTAL_RAM{4096<<20}`. — [node/caches.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/caches.cpp)
- **v31, PR #31645:** the default LevelDB write batch (`-dbbatchsize`, a DEBUG_ONLY option) rose from 16 MiB to **32 MiB**. The PR was merged on 2025-10-31. Results:
  - Raspberry Pi at dbcache=500: "~30% faster" flushes, at the cost of about 70–75 MiB more peak memory.
  - reindex-chainstate at dbcache=10000: 25016 s → 24801 s (1.01×).
  - — [PR #31645](https://github.com/bitcoin/bitcoin/pull/31645). Code: `DEFAULT_DB_CACHE_BATCH{32_MiB}`, and txdb writes a partial batch whenever `batch.ApproximateSize() > batch_write_bytes`. — [kernel/caches.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/kernel/caches.h), [txdb.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/txdb.cpp), [init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- **Split of the dbcache budget:**
  - txindex takes min(total/8, 1024 MiB) if enabled.
  - Filter indexes take min(total/8, 1024 MiB) shared across indexes, if enabled.
  - Then block-tree DB takes min(total/8, 2 MiB), coins LevelDB cache takes min(total/2, **8 MiB**), and **the rest goes to the in-memory coins cache**.
  - With `blockfilterindex=1` and dbcache=2048, about 256 MiB would go to the filter index.
  - — [node/caches.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/caches.cpp), [kernel/caches.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/kernel/caches.h)
- **Compaction:** after a completed flush, `ShouldCompactChainstate` triggers `CompactFull()` with probability 1/320, "Roughly every 2 weeks with hourly flushes", and **only when not in IBD**. — [validation.cpp v31.1](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **dbcache-scaling benchmarks** (l0rinc, `-reindex-chainstate` to ~936,639; times for 450 / 1000 / 2000 / 3000 MiB):

  | Machine | 450 | 1000 | 2000 | 3000 |
  |---|---|---|---|---|
  | i9-9900K SSD | 22077 s | 19725 s | 18302 s | 17859 s |
  | M4 Max (to 900k) | 20261 s | 11575 s | 8227 s | 7064 s |
  | Umbrel N150 | 33619 s | — | 28353 s | 27910 s |

  - The author: "450-1024-2048 jumps are all very relevant after which the effects taper off" and "the 2 GiB to 3 GiB speedup is negligible".
  - The PR was moved to draft and was not merged; darosior and willcl-ark left Concept NACKs.
  - — [PR #34641](https://github.com/bitcoin/bitcoin/pull/34641)
- **Large cache, full IBD (2023 data):** prune=0 at dbcache=450 took 7:08:07, and at dbcache=16384 it took 4:51:53, which is **~1.47× faster**. — [PR #28280](https://github.com/bitcoin/bitcoin/pull/28280)
- **Pending work, not in v31.1:**
  - The IBD tracking PR #32043 reports ">20% speedup for full IBD" from unmerged changes, measured at 5 GiB dbcache. — [PR #32043 mirror](https://mirror.b10c.me/bitcoin-bitcoin/32043/)
  - PR #35195, which caches outpoint hash codes, is open and gives 4–11% faster loads. — [PR #35195 mirror](https://mirror.b10c.me/bitcoin-bitcoin/35195/)
  - v31.1 contains a `CoinsViewOverlay` used for `m_connect_block_view`, but no parallel input fetcher, from a grep of the source. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)

### Inferences
- **Wipe frequency at the current cache size:** at the observed ~0.67 MiB/block of net growth, the 1936 MiB snapshot cache would hit the LARGE/CRITICAL threshold about every ~2,500–3,000 blocks. Over ~39.5k blocks that is roughly **13–15 cache wipes**, each followed by a cold-cache phase like the one seen at activation. Prune Syncs drop spent entries, which slows growth, so the real count is likely somewhat lower. This is an extrapolation from about 90 blocks of data.
- **Effect of a bigger cache:** dbcache of ~8–10 GiB would cut this to roughly 2–4 wipes. A cache of ~25 GB would probably avoid wipes entirely for this range. The expected gain on this range is somewhere between the 2→3 GiB "taper" figures (2–14%) and the 450→16384 full-IBD gain (1.47×). No benchmark covers this exact case, a snapshot start with an empty cache on NVMe.

### Gaps
- No public benchmark measures dbcache scaling specifically for snapshot-to-tip sync, and none goes above 3 GiB on v30/v31.
- The exact release that shipped #30611 (hourly writes) was not confirmed. The v31.1 source does contain it.

## Q2: What dbcache holds the whole UTXO set at ~935k–975k, are there diminishing returns, and what are the risks?

### Takeaway
At ~143 bytes per coin, the 164.24M-coin set at 935k needs **~23.5 GB (≈22 GiB) of coins cache**, a little more by 975k. That does not fit in ~21 GB available. On disk the set takes 11 GB. Returns flatten above ~2–3 GiB for reindex-chainstate. The main risks are OOM against the cgroup limit, which also counts page cache, and wipe flushes when the cache fills.

### Cited Findings
- The snapshot at height 935,000 has **164,241,311 coins**. 14.76M entries occupied 2116 MB of cache, which is ~143 B/coin. — [Live node](#)
- An AssumeUTXO load with a "very large dbcache" peaked at **24.11 GB** of baseline memory. 4.64 GB at dbcache=4500 and 744.5 MB at dbcache=450 were also reported. — [PR #35195 mirror](https://mirror.b10c.me/bitcoin-bitcoin/35195/). This fits ~23.5 GB of coins plus overhead.
- The snapshot chainstate LevelDB on disk is **11 GB**. — [Live node](#)
- PR #28280 notes about 12% more memory at maximum dbcache because of the two extra pointers per entry. Max RSS was 16.5 GB at dbcache=16384. — [PR #28280](https://github.com/bitcoin/bitcoin/pull/28280)
- v31.0 warns that the auto-default may OOM in containers. In #34641 reviewers raise container memory detection: it measures "the hosts memory, not whatever the system administrator allocated for the container". l0rinc reports OOMs on 2 GB machines "on some flushes". — [v31.0 release notes](https://raw.githubusercontent.com/bitcoin/bitcoin/master/doc/release-notes/release-notes-31.0.md); [PR #34641](https://github.com/bitcoin/bitcoin/pull/34641)
- Before each write, bitcoind requires free disk of at least `48*2*2*dirty_count` bytes. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **Crash exposure:** coins are written at every prune event, every 50–70 min, and at every cache-full wipe, so at most ~1 h of validation can be lost. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)

### Inferences
- **Size of the working set:** the post-snapshot cache starts **empty** (activation logged "(0 MB)"), so the useful working set is the coins created or touched in 39.5k blocks, not the whole 164M-coin set. A cache of ~8–12 GiB plausibly covers most of the 39.5k-block range with few wipes.
- **Shutdown flush:** with prune Syncs every few dozen blocks, the dirty set stays small, so the final shutdown flush is short even with a large dbcache.
- **Memory limit:** the container limit must grow with dbcache. A safe rule from the live sample is mem_limit ≥ dbcache + ~1.5 GiB anon overhead + several GiB of page cache for the 11 GB LevelDB.

### Gaps
- The exact UTXO count at 974.5k was not measured; it is likely ~165–170M coins.
- No measured breakdown of glibc fragmentation during wipe flushes. Brief peaks above dbcache during a flush are reported anecdotally (l0rinc OOM comment) but not quantified.

## Q3: Does pruning force cache flushes during IBD, how often at prune=10000 compared with other targets, and how does it interact with assumeutxo?

### Takeaway
Since v28, prune events trigger a non-wiping Sync instead of a wipe. With prune=10000 in snapshot IBD, however, they happen **about once per completed 128 MiB block file, roughly every ~60–80 blocks or ~500 times over this range**. This is because the IBD buffer (1 MB × remaining blocks) is larger than the halved 5,000 MiB target. Each Sync writes all dirty coins and loses the "FRESH" savings. This node has already pruned, so **prune=0 would require a full `-reindex`**. A much larger prune target during the sync is allowed and reduces prune events.

### Cited Findings
- **When the prune check runs:** `m_check_for_pruning = true` is set whenever a new block-file chunk (`BLOCKFILE_CHUNK_SIZE` 16 MiB) or undo chunk (`UNDOFILE_CHUNK_SIZE` 1 MiB) is allocated in prune mode. The next `FlushStateToDisk` then runs `FindFilesToPrune`. If files are selected, `fFlushForPrune` → write → `Sync()`. — [node/blockstorage.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.cpp), [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **Target with assumeutxo:** `target = max(MIN_DISK_SPACE_FOR_BLOCK_FILES (550 MiB), PruneTarget / num_chainstates)`, where num_chainstates = 2 while a historical (background) chainstate exists. The comment says "half of the block storage will be reserved for the historical chainstate". **prune=10000 therefore means a 5,000 MiB target during this sync.** — [node/blockstorage.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.cpp)
- **IBD buffer:** `nBuffer = 16 MiB + 1 MiB`. In IBD, when the best header is above the tip, it adds `1,000,000 bytes × (best_header − tip)`. The source comment: "On a prune event, the chainstate DB is flushed. To avoid excessive prune events negating the benefit of high dbcache values, we should not prune too rapidly." Files are pruned in order until `usage + buffer < target`. — [node/blockstorage.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.cpp). This was introduced by PR #20827 (merged 2024-01-25), which replaced a 10%-of-target buffer. Its results:
  - flushes 178 → 69 (prune=4400, dbcache=300)
  - full IBD to 710k: +15.6% (prune=4400, dbcache=4000) and +17.4% (prune=8800, dbcache=4000)
  - andrewtoth: 21% faster at dbcache=2000, prune=2000
  - — [PR #20827 mirror](https://mirror.b10c.me/bitcoin-bitcoin/20827/)
- **Which blocks may be pruned:** `GetPruneRange`. For an unvalidated snapshot chainstate, `prune_start = snapshot_base + 1`, so blocks at or below 935,000 are kept for background validation. `prune_end = min(lock height, tip − MIN_BLOCKS_TO_KEEP)` with `MIN_BLOCKS_TO_KEEP = 288`. The comment notes that blockfilterindex needs the trailing undo window. A file is pruned only when its entire height range lies inside that window. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp), [validation.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.h)
- **Prune locks** (from indexes) cap the last prunable height at `height_first − PRUNE_LOCK_BUFFER(10) − 1`. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **Separate files:** BlockManager keeps separate block-file cursors per `BlockfileType` (NORMAL vs ASSUMED). Snapshot-range and background-range blocks go to different files, so a file does not mix both. — [node/blockstorage.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.h)
- `MAX_BLOCKFILE_SIZE = 0x8000000` (128 MiB). — [node/blockstorage.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.h)
- **Going back to unpruned:** if a node has pruned and then runs unpruned, loading fails with "You need to rebuild the database using -reindex to go back to unpruned mode. This will redownload the entire blockchain". — [node/chainstate.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/chainstate.cpp)
- **Cache split between chainstates (`MaybeRebalanceCaches`):** while the node is in IBD with both chainstates, the snapshot (current) chainstate gets 95% and the historical one 5%. Once out of IBD, this flips to historical 95% and snapshot 5%. After background validation completes, all cache goes to the snapshot chainstate. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp). The live log matches: 1936.1 MiB is 95% of 2038 MiB. — [Live node](#)
- **Measured cost of pruning with a warm cache (2023, pre-#20827):** at dbcache=16384, prune=550 took 6:01:00 with the Sync fix, against 4:43:11 for prune=0. **Unpruned was ~22% less time (1.28×).** — [PR #28280](https://github.com/bitcoin/bitcoin/pull/28280)
- **PR #20827 caveat:** the 1 MB-per-block assumption "doesn't hold" on non-mainnet networks. — [PR #20827 mirror](https://mirror.b10c.me/bitcoin-bitcoin/20827/)

### Inferences
- **prune=10000 now (target 5,000 MiB):** the buffer is ~39.5 GB at the start and much larger than the target until the last ~4–5k blocks. Every check prunes every eligible file, which is every snapshot-range file older than tip−288. About 288 blocks (~0.5 GB) are always retained, so in practice **one prune Sync happens per new 128 MiB blk file**. At about 1.6–2 MB/block that is about every 65–80 blocks, or **~500–550 Syncs** for ~65–70 GB of blocks. Each Sync writes every coin created since the previous one that is still unspent. Coins that would otherwise have been created and spent inside the cache without ever touching disk (FRESH) now cost a write plus a later delete.
- **Larger prune target during the sync:** for example prune=100000, which gives a 50,000 MiB target. With 4 GB in use and a 39.5 GB buffer, the first ~9k blocks see **no prune events at all**, because usage grows ~1.7 MB/block and the buffer shrinks 1 MB/block. After that the node prunes only "enough" files each time, roughly one file per ~180 blocks. That is about a **2–3× reduction in prune Syncs**. To never prune during this range, the target per chainstate must exceed total block usage at the tip (~4 + ~70 GB). That means **prune ≳ 150,000 MiB**, which needs ~75 GB of block disk. That fits the 203 GB free, but tightly with other services.
- **Lowering the target afterwards:** raising the prune target and later lowering it back to 10000 is allowed, because it stays in prune mode. The next start prunes down in one event. prune=0 is **not** an option here: the node has already pruned (blocks dir is 3.9 GB, ibd chainstate at 269k), so it would need a full `-reindex`.
- **Expected gain from fewer prune Syncs:** roughly 10–25%, based on the 1.28× unpruned-vs-pruned result at a large dbcache and the 15–21% from #20827's frequency reduction. The gain is larger with a larger dbcache. At a 2 GiB cache, wipe flushes dominate and the prune Sync savings matter less.
- **After the snapshot reaches the tip:** IBD becomes false and the cache split flips to 95% for the background chainstate. Allod's watcher then restarts bitcoind with the normal profile (BITCOIN_DBCACHE default 450, mempool on). The background chainstate (269k → 935k) then validates with a small cache. This is outside this question but large in total time.

### Gaps
- No public benchmark of prune-event frequency during snapshot-to-tip sync specifically.
- How aggressively v31.1 downloads background-range blocks during snapshot IBD (net_processing `TryDownloadingHistoricalBlocks`) was not verified. Those blocks also add to `CalculateCurrentUsage`, which counts all files.
- No PR newer than #20827 or #28280 that further reduces prune flushes was found in v29–v31.

## Q4: Do LevelDB settings, filesystem, Docker storage or the I/O scheduler matter on NVMe?

### Takeaway
LevelDB tuning is mostly fixed in code. The only user knob is the hidden `-dbbatchsize`, now 32 MiB, which gives small gains on fast hardware. `max_open_files`, write buffer and compression are set internally. This node already uses a bind mount (no overlayfs), ext4 with relatime, and the `none` scheduler. No benchmarks show ext4 vs XFS vs btrfs or noatime making a difference.

### Cited Findings
- **LevelDB options:**
  - `block_cache = nCacheSize/2`
  - `write_buffer_size = nCacheSize/4` ("up to two write buffers may be held in memory simultaneously")
  - `compression = kNoCompression`
  - `max_file_size = max(default, DBWRAPPER_MAX_FILE_SIZE)`, where `DBWRAPPER_MAX_FILE_SIZE = 32 << 20` (32 MiB)
  - `max_open_files` is the default (1000) on most platforms, with 64 as a fallback on some
  - The coins DB nCacheSize is capped at 8 MiB (`MAX_COINS_DB_CACHE`), so its block cache is ≤4 MiB and its memtable ≤2 MiB. **LevelDB's own caching is tiny; the OS page cache does the read caching.**
  - — [dbwrapper.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/dbwrapper.cpp), [dbwrapper.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/dbwrapper.h), [kernel/caches.h](https://github.com/bitcoin/bitcoin/blob/v31.1/src/kernel/caches.h)
- **`-dbbatchsize`:** "Maximum database write batch size in bytes (default: 33554432)", marked DEBUG_ONLY. A Knots backport reported flush times of 22m46s at 16 MiB vs ~14m43s at 64 MiB (dbcache=30000). Core chose 32 MiB to limit memory peaks. On fast hardware: 1.01× for reindex-chainstate. — [init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp); [PR #31645](https://github.com/bitcoin/bitcoin/pull/31645)
- **LevelDB regression issue #35457 (closed 2026-07-13):** partial IBD regression after a LevelDB subtree bump that reverted the read-only mmap limit from 4096 to 1000 on 64-bit and disabled seek compaction. It hits chainstates with many small, pre-#30039 2 MB SST files, such as datadirs part-synced on ≤28.0. Workaround: hidden `-forcecompactdb`. Follow-ups: #35465 "coins: compact chainstate regularly" and #35467 "warn about legacy chainstate table format". — [Issue #35457 mirror](https://mirror.b10c.me/bitcoin-bitcoin/35457/)
- One older report (#21095) found that moving chainstate to a fast SSD helped "Connect block" times. I found **no ext4/XFS/btrfs or noatime comparisons** with numbers. — [Issue #21095 mirror](https://mirror.b10c.me/bitcoin-bitcoin/21095/)
- Live: ext4 `rw,relatime`, NVMe scheduler `[none]`, and `/data` is a bind mount of the host partition. — [Live node](#)
- **Flush throughput during the load:** each ~2.1 GB flush took 8–37 s, which is ~57–265 MB/s of logical data. This is far below the drive's ~2.5 GB/s. — [Live node](#)

### Inferences
- **Who sets the speed:** flush speed is bound by single-threaded LevelDB serialisation and write work, plus compaction stalls (the 8–37 s spread suggests L0 compaction back-pressure), not by NVMe bandwidth. Filesystem or scheduler changes are unlikely to give measurable gains on this NVMe.
- **Snapshot LevelDB layout:** the snapshot chainstate was freshly written by v31.1 with 32 MiB files, so the #35457 legacy-layout problem should not apply to it. The background `/data/chainstate`, written earlier on the same binary, is small (425 MB).
- **Docker:** overlayfs is irrelevant here because data is on a bind mount. The relevant Docker factor is the **memory cgroup limit**, which evicts the page cache that LevelDB reads depend on.

### Gaps
- No quantitative data for filesystem, mount options or the I/O scheduler on modern Core versions.
- I could not confirm the release of #30039 (32 MiB max file size). It is in v31.1.

## Q5: Does `-blocksdir` on a separate disk or `-reindex-chainstate` matter here?

### Takeaway
`-reindex-chainstate` is **not usable**: it is rejected in prune mode and would wipe the snapshot chainstate. A separate `-blocksdir` has no documented benefit on a single fast NVMe. Sequential block writes are small next to the random reads and writes on the chainstate.

### Cited Findings
- With pruning on, init returns: "Prune mode is incompatible with -reindex-chainstate. Use full -reindex instead." The help text says: "If an assumeutxo snapshot was loaded, its chainstate will be wiped as well. The snapshot can then be reloaded via RPC." — [init.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/init.cpp)
- The block write path flushes block and undo files before each chainstate write (`FlushChainstateBlockFile`) and checks free space on `blocks_dir`. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)

### Inferences
- **Block-data volume:** 65–70 GB over the sync is ~1.7 MB/block. At 1 block/s that is ~2 MB/s, under 0.1% of NVMe bandwidth. Moving blocks elsewhere would only help if the chainstate disk were saturated, which the flush numbers above do not show. The more useful change is a large page cache so chainstate reads hit RAM.

### Gaps
- No benchmarks found for a split blocksdir on modern Core with NVMe.

## Q6: How does the loadtxoutset load (FlushSnapshotToDisk) depend on dbcache?

### Takeaway
The load checks the cache every 120,000 coins and does a full wiping `Flush()` once usage passes the snapshot chainstate's cache budget. Every coin is written to LevelDB exactly once whatever the cache size, so total write work is constant. A larger dbcache cuts the number of flushes (11 at 2 GiB) but measurably saves only ~0–6% of load time. Holding the whole set in memory, avoiding intermediate flushes, would need ~23.5 GB.

### Cited Findings
- **Load loop:** in `PopulateAndValidateSnapshot`, every `coins_processed % 120000 == 0` (the comment says "<5MB of memory imprecision") the code checks `GetCoinsCacheSizeState()`. At ≥ CRITICAL it calls `FlushSnapshotToDisk(coins_cache, false)`, which logs and calls `coins_cache.Flush()`, a full write plus clear. After the last coin, `FlushSnapshotToDisk(…, true)` performs the final save. — [validation.cpp v31.1](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **Budget during the load:** the snapshot chainstate is created with `mempool=nullptr`, so there is no mempool bonus. Its caches are `current_coinstip_cache_size * SNAPSHOT_CACHE_PERC` and `current_coinsdb_cache_size * SNAPSHOT_CACHE_PERC`. Meanwhile the active (ibd) chainstate is shrunk to `IBD_CACHE_PERC`. — [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp). Live: the ibd chainstate was shrunk to 20.4 MiB (≈1%), and the snapshot cache filled to 2116 MB before each flush. — [Live node](#)
- **Load time vs dbcache (l0rinc, loadtxoutset):** dbcache=500: 451.7 s; 3000: 426.1 s; 4000: 440.7 s. Peak memory was 744.5 MB at 450, 4.64 GB at 4500 and 24.11 GB at a "very large" dbcache. — [PR #35195 mirror](https://mirror.b10c.me/bitcoin-bitcoin/35195/). Separately, assumeutxo loads of 1h46m at dbcache=450 and 1h41m at dbcache=375 with 32 MiB batches were reported. — [PR #31645](https://github.com/bitcoin/bitcoin/pull/31645)
- **Not wiping on stats/scans:** PR #30610 proposed a non-wiping `FORCE_SYNC` for `scantxoutset`, `gettxoutsetinfo` and `dumptxoutset`, and did not change `loadtxoutset`. It was closed, and the work was revived as #33680, reported merged. v31.1 has both `FORCE_SYNC` and `FORCE_FLUSH` modes in `FlushStateToDisk`. — [PR #30610 mirror](https://mirror.b10c.me/bitcoin-bitcoin/30610/); [validation.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/validation.cpp)
- **Live result:** 11 intermediate flushes, 252 s of 641 s (39%). The cache was empty ("0 MB") at activation, so the snapshot→tip sync starts with a cold cache whatever the dbcache. — [Live node](#)

### Inferences
- **Load time:** because all 164M coins must reach LevelDB anyway, a larger dbcache mainly removes per-flush fixed costs and compaction interplay. Expect at most a ~5–10% faster load from 2 → 8 GiB. The ~39% "flush share" is not avoidable time; it is the unavoidable write work, front-loaded.
- **Number of flushes:** at dbcache 8 GiB (~7.6 GiB snapshot coins cache, ~55M coins per flush) the load would make about 2 intermediate flushes plus the final one. At 12 GiB it would make 1 plus the final one. Only ≥ ~23.5 GB avoids all intermediate flushes, and even then the final save writes everything.
- **No warm start:** the load wipes its cache at the end, so the sync after activation cannot benefit from coins loaded into memory during the load.

### Gaps
- No upstream PR found that keeps the loaded snapshot coins warm in the cache after activation, for example by using Sync at the end.

## Recommended settings and expected speedups for this machine (synthesis)

### Takeaway
1. Raise the container memory limit and dbcache together. For example, set `BITCOIN_MEMORY_LIMIT=14g` and dbcache ≈ 8000–10000 MiB for the snapshot→tip sync, then return to the normal profile.
2. Raise the prune target temporarily, for example `prune=100000`–`150000` if disk allows, and lower it to 10000 after the sync. prune=0 needs a reindex here.
3. Keep blocksonly=1, and avoid restarts mid-sync because each restart cold-starts the cache.
4. Leave LevelDB, filesystem and scheduler alone.

### Cited Findings
- Each lever is cited in Q1–Q6:
  - dbcache scaling, 1.18–2.46× for 450→2000 and diminishing above 3 GiB — [PR #34641](https://github.com/bitcoin/bitcoin/pull/34641)
  - 1.47× for 450 → 16384 on full IBD, and 1.28× for prune=0 vs prune=550 at 16 GiB — [PR #28280](https://github.com/bitcoin/bitcoin/pull/28280)
  - 15–21% from fewer prune flushes — [PR #20827 mirror](https://mirror.b10c.me/bitcoin-bitcoin/20827/)
  - halved prune target with 2 chainstates — [node/blockstorage.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/blockstorage.cpp)
  - prune=0 requires reindex — [node/chainstate.cpp](https://github.com/bitcoin/bitcoin/blob/v31.1/src/node/chainstate.cpp)

### Inferences
- **dbcache 2048 → ~8–10 GiB plus mem_limit 6 → ~14 GiB.** The limit must grow too: the live anon RSS was ~3.45 GB at dbcache 2 GiB, and page cache for the 11 GB LevelDB needs room.
  - Fewer wipe flushes: from ~13–15 down to ~2–4.
  - More page cache for cold lookups after activation and after each wipe.
  - **Estimated 15–35% faster snapshot→tip sync** if lookup- or flush-bound. This sits between the 2→3 GiB taper and the 450→16 GiB gain; it is an estimate, not measured.
  - Allod's `sync_dbcache` 2048 cap and its `memory.current`-based headroom (which counts page cache) would both need changing.
  - With ~21 GB available, ≤12 GiB dbcache inside a ≤16 GiB limit leaves host margin.
- **prune 10000 → 100000 for the sync (~50 GB block budget):** removes prune Syncs for the first ~9k blocks and about halves them afterwards. **Estimated +5–15%**, with more of the gain at larger dbcache. prune ≳150000 (~75 GB of blocks) removes nearly all of them; set it back to 10000 afterwards, and the next start prunes in one event.
- **loadtxoutset itself:** a larger dbcache reduces 11 flushes to 1–2, but total write work is the same. **Expect ≤5–10%** (~30–60 s of a ~13.5 min load). Low priority.
- **Already right, or no measurable gain expected:**
  - Keep blocksonly=1; there is no mempool bonus either way.
  - Keep blockfilterindex off during the sync if possible: it costs dbcache/8 (≤1 GiB) and adds a prune lock plus index I/O.
  - Ext4, bind mount and the `none` scheduler are fine.
  - Don't use `-reindex-chainstate`.
  - Don't move blocksdir.
  - `-dbbatchsize` 32 MiB is the default; 64 MiB may give a small flush gain at ~+70 MiB peak. Unmeasured on this hardware.
- **Network floor:** at ~30 MB/s, downloading 65–70 GB takes ≥ ~36–39 min. The current ~1 s/block (~11 h for 39.5k blocks) is far from network-bound, so the levers above apply.

### Gaps
- None of the expected-speedup figures are measured on this exact workload. The ~1 s/block early rate is from the first ~150 blocks after activation with a cold cache and may improve on its own as the cache warms.
