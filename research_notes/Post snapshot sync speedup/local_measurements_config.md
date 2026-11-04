# Local measurements and config audit: allod2-bitcoind-1 (Bitcoin Core v31.1) syncing past an AssumeUTXO snapshot (base height 935,000)

Measured on 2026-11-04 (UTC), host: Intel Core Ultra 7 155H (22 threads), 30.85 GiB RAM, Kioxia NVMe (ext4, `rw,relatime`, mq-deadline), rootless Docker (overlay2, cgroup v2), kernel 7.0.0-38.
Source conventions: "Obs:" = direct read-only observation on this machine (command, UTC time). Repo files are cited as file links. Raw sampler logs: `/tmp/btcsamp/samples.log`, `/tmp/btcsamp/samples2.log`, `/tmp/btcsamp/post.log` (debug.log excerpt from 09:21:59 to 09:30:35).
**Important caveat:** the whole `allod2` stack (all containers) was removed by an external actor between 10:00:40 and 10:00:58 UTC. This study never stopped or reconfigured anything. Volumes survive (`allod2_bitcoind-data` contains `.fastsync-done`). After 10:00 no further measurement was possible, and debug.log in the volume is unreadable from the host (rootless subuid ownership).

## 1. Current state and timeline (getchainstates / getblockchaininfo / peers / network)

### Takeaway
loadtxoutset finished at 09:21:56. The snapshot validated and activated, and networking resumed at 09:21:59. Tip sync ran from 935,001 to 939,725 by 10:00 (about 2.07 blocks/s on average). The node was never network-bound. At first it was bound by the single validation (message handler) thread plus cold-cache disk reads. After the assumevalid height (938,343) it was bound by CPU script checks on a CPU clamped to about 0.4–1.3 GHz.

### Cited Findings
- Timeline from debug.log and helper logs. Obs: `grep` of `/data/debug.log` and `docker logs`, 09:19–09:30:
  - 08:43:34: bitcoind starts. `Using the 'x86_shani(1way;2way)' SHA256 implementation`. Cache config: 2.0 MiB block index, 8.0 MiB chainstate DB, `2038.0 MiB for in-memory UTXO set`. `Script verification uses 15 additional threads`. `Assuming ancestors of block 00000000000000000000ccebd6d74d9194d8dcdc1d177c478e094bfad51ba5ac have valid signatures`. `Prune configured to target 10000 MiB`.
  - 08:45:34: headers synced to 974,508 (~2 min after start). The snapshot base header was therefore available long before fast sync was requested.
  - 08:43–08:59:15: normal IBD from genesis reaches 269,636 (cache 1153.6 MiB, 8.63 M txo). 3 "Peer is stalling block download" disconnects (08:49:17, 08:49:59, 08:55:29). No UpdateTip between 08:59:15 and 09:03:50 (cause unknown, see Gaps).
  - 09:03:50: `SetNetworkActive: false` (assumeutxo.sh pauses networking once the base header is known and the download has started). Entrypoint log: `downloading https://files-vps02.jaonoctus.dev/utxo-935000.dat from byte 0 of 9387990306`. utxo-snapshot.dat mtime 09:08, so the 8.8 GB downloaded in about 4.5–5 min (~30 MB/s, matching the link).
  - 09:08:22–09:19:08: loadtxoutset loads 164,241,311 coins. 12 `FlushSnapshotToDisk` flushes of the ~2.1 GB cache took 23.6, 18.4, 30.3, 10.6, 30.6, 36.4, 8.0, 26.6, 8.9, 37.3, 21.6 and 12.7 s, about **265 s in total (~4.4 min)** of the ~10.7 min load. Each flush logged `Cache size (2116229760) exceeds total space (2115627909)` / `Flushing large (14760000 entries) UTXO set`.
  - 09:19:21–09:21:56: silent "checking" phase (UTXO-set hash). top -H showed one `b-http_worker` thread at 99% CPU, everything else idle. Lasted **2 min 35 s**.
  - 09:21:56: `[snapshot] validated snapshot` / `successfully activated snapshot`. Cache split: snapshot chainstate coinstip **1936.1 MiB**, background (ibd) chainstate **101.9 MiB**, coinsdb 7.6 / 0.4 MiB.
  - 09:21:59: `SetNetworkActive: true`. 09:22:02: first UpdateTip at 935,001.
- At 09:19:04 (before activation), getchainstates showed one validated chainstate at 269,636 (`coins_tip_cache_bytes` 21,369,978), headers 974,509, `networkactive: false`, 0 connections, `pruned: true`, `pruneheight 243524`, `size_on_disk` 3.82 GB. Obs: bitcoin-cli getchainstates/getblockchaininfo/getnetworkinfo, 09:19:04.
- After activation, two chainstates: background `validated=true` at 269,636→270,599, and snapshot `validated=false`. Obs: getchainstates sampler, 09:22–10:00.
- Snapshot-chain rate by window. Obs: sampler every ~17 s, getchainstates + docker stats:

| Window (UTC) | Heights | blocks/s | Docker NET RX MB/s | avg container CPU |
|---|---|---|---|---|
| 09:22:01–09:25:44 | 935,002→935,167 | 0.74 | 7.2 | 89% |
| 09:26:01–09:30:51 | 935,176→935,728 | 1.90 | 1.7 | 84% |
| 09:36:15–09:40:59 | 936,410→937,191 | 2.75 | 4.2 | 87% |
| 09:41:17–09:45:41 | 937,223→937,813 | 2.23 | 3.8 | 98% |
| 09:46:00–09:49:40 | 937,864→938,256 | 1.78 | 2.7 | 82% |
| 09:50:00–09:54:47 (past assumevalid) | 938,308→939,013 | 2.46 | 4.2 | **516%** |
| 09:55:05–09:59:59 | 939,033→939,725 | 2.35 | 4.1 | **614%** |
| **09:22:01–09:59:59 overall** | 935,002→939,725 | **2.07** | **3.6** | 242% |

- Per-minute UpdateTip counts on the snapshot chain from debug.log: 32, 79, 47, 18, 49, 77, 119, 137, 112 blocks/min from 09:22 to 09:30. Cache use at 935,670 was `cache=342.2MiB(2615509txo)`, so it warmed at ~0.5 MiB/block. Obs: `/tmp/btcsamp/post.log`, 09:30:35.
- The background chainstate went 269,636→270,599 between 09:26 and 09:34, then **stalled at 270,599** until 10:00. Obs: sampler.
- Peers: 1 at 09:22:01, 10 from 09:22:58 (8 outbound-full-relay + 2 block-relay-only). Blocks in flight rose to **159** (16 per peer, i.e. every slot full) by 09:24:47 and stayed there. Total bytesrecv from peers was 2,219 MB at 09:28:34 and stayed **flat 09:28:34–09:29:03** with all slots full. Ping times rose to **30–174 s** (e.g. peer 32 61,762 ms, peer 42 174,240 ms), meaning the node was not servicing peer messages promptly. Obs: getpeerinfo sampler, 09:22:24–09:29:29.
- Thread view: during 09:22–09:33, `b-msghand` (validation / message handler) ran at 36–97% and `b-net` at 5–63%. All 15 `b-scriptch.N` threads sat in `futex_do_wait` (idle), as expected below the assumevalid height. Obs: top -H and /proc/<tid>/wchan, 09:22–09:30.
- Assumevalid block 00…ba5ac is at **height 938,343** (time 1772055173). Blocks 935,001–938,343 skip script checks; 938,344→tip (~36,170 blocks) need full script verification. Obs: getblockheader, 09:20. This matches the CPU step from ~85% to 500–800% at ~09:50 (height ~938,3xx).
- Blockfilterindex: `blockfilterindex=0` in the live bitcoin.conf (env `BITCOIN_BLOCKFILTERINDEX=0` in this deployment), so there was no index activity. Obs: `cat /data/bitcoin.conf`, container env.
- No prune events appeared in the 09:21:59–09:30:35 log excerpt. Obs: post.log.

### Inferences
- **Phase A (935,001–~935,200, first ~4 min):** the cache was cold right after activation (the snapshot load flushed it to disk), so every UTXO lookup went to LevelDB. Validation ran at only 0.74 blocks/s. Network was not limiting: 159 blocks in flight, peers' sockets were paused because msghand could not keep up (rising pings, flat bytesrecv).
- **Phase B (~935,200–938,343):** 1.8–2.75 blocks/s with no script checks. The single msghand thread was the bottleneck, slowed by a clamped CPU clock (§3) and cache-miss disk reads (§2).
- **Phase C (938,344→tip):** script checks enabled. CPU rose to 5–8 cores but throughput stayed ~2.4 blocks/s. The CPU is clamped to ~0.4–1.3 GHz most of the time, so 15 script threads are doing the work of ~3–4 full-speed cores. This phase is CPU-bound.
- The 20 min of from-genesis IBD before fast sync was chosen only helps the background chainstate (it already stood at 269,636), not time-to-tip.

### Gaps
- No debug.log after 09:30:35 (the stack was removed at ~10:00:45 and the volume file is unreadable from the host). No observation of cache-full flushes during tip sync, prune events, or any "stalling" message after activation.
- Cause of the 08:59:15–09:03:50 gap with no blocks connected is unknown. It may be the snapshot download (which starts ~09:03) or a request-window stall; not confirmed.
- Why the background chainstate stopped at 270,599 was not determined.

## 2. Container: cgroup, memory, storage

### Takeaway
No CPU quota. The 6 GiB hard memory limit was the binding constraint: the container sat exactly at memory.max. Page cache was squeezed to ~2.7 GB against an 11 GB snapshot chainstate, causing page-cache thrash, sustained 140–420 MB/s NVMe reads, and socket-memory throttling.

### Cited Findings
- `NanoCpus=0 CpuQuota=0 CpuPeriod=0 CpusetCpus=` (no CPU limit); cgroup `cpu.max = max 100000`. `Memory=6442450944` (6 GiB), `MemorySwap=6442450944` (no swap). Obs: docker inspect / cgroup files, 09:19 and 09:30.
- `/data` is a **named volume** `allod2_bitcoind-data` at `/home/fmar/.local/share/docker/volumes/allod2_bitcoind-data/_data`, on `/dev/nvme0n1p1` ext4 `rw,relatime`. 836 GB disk, 203 GB free (75% used). Storage driver overlay2 (backing extfs), rootless context, cgroup v2 via systemd. Obs: docker inspect, findmnt, df, docker info, 09:19:53.
- On-disk sizes at 09:19: blocks 3.7 GB, chainstate 426 MB, chainstate_snapshot 11 GB, utxo-snapshot.dat 8.8 GB (deleted after load). Obs: du.
- cgroup at 09:30:08: `memory.current 6442360832` vs `memory.max 6442450944` (at the limit), `memory.peak 6446190592`, anon 3.49 GB, file (page cache) 2.75 GB (active 0.71 GB), `workingset_refault_file 33,032,351`, `pgmajfault 985,726`. `memory.events`: `max 1,074,219`, `oom 0`, `oom_kill 0`, **`sock_throttled 42,626`**. `memory.pressure some/full avg60 1.56%`, `io.pressure full avg60 4.83%`. Obs: cgroup files, 09:30.
- docker stats memory oscillated 3.4–5.98 GiB of 6 GiB during tip sync. Obs: sampler 09:22–10:00.
- NVMe reads 72 MB/s (11.9% util) during the hash phase. During tip sync, 140–425 MB/s reads and 0–11 MB/s writes. Obs: iostat -dxm nvme0n1, 09:20–09:33.
- The entrypoint computed `initial sync profile (5367 MiB headroom, 22 CPUs): dbcache=2048 blocksonly=1`. Obs: docker logs.

### Inferences
- Memory is not a correctness risk (no OOM), but the limit hurts throughput. bitcoind's anonymous memory (dbcache budget + allocator retention from the snapshot load + block buffers) leaves ~2–2.7 GB for page cache. The 11 GB LevelDB snapshot chainstate is re-read from NVMe on every cache miss (33 M file refaults). TCP receive was throttled 42 k times by cgroup memory pressure.
- The NVMe is not saturated (io.pressure full ~4%, util low). Disk costs latency on the single msghand thread rather than bandwidth.

### Gaps
- `/proc/<pid>/io` is unreadable under rootless Docker, so per-process read/write bytes were not measured; host-wide iostat was used instead and may include other processes.

## 3. Host CPU: frequency, power, thermal

### Takeaway
The CPU is heavily clamped. A pinned P-core busy loop ran at **600 MHz** (max 4,800 MHz) while the package was only 63–66 °C. The USB-C PD source reports **5 V × 3 A (15 W)** negotiated, out of 20 V / 3.25 A (~65 W) capable, and the battery reads "Not charging". This is the dominant hardware bottleneck for both the single-threaded phases (hash, msghand) and the script-check phase.

### Cited Findings
- Governor `powersave` (intel_pstate `active`, HWP), EPP `performance`, `powerprofilesctl get` = `performance`, ACPI platform_profile `performance`, `no_turbo 0`, `max_perf_pct 100`, `min_perf_pct 8`. scaling_max_freq 4.5/4.8 GHz on P-cores, 3.8 GHz E-cores, 2.5 GHz LP-E. RAPL PL1 = PL2 = 64 W, peak 140 W. Obs: /sys/devices/system/cpu, /sys/class/powercap, 09:19–09:21.
- `scaling_cur_freq` was mostly 400–1,300 MHz on all cores while bitcoind had a thread at 99–100%. There were brief bursts to 3.6–4.2 GHz (09:23:43, 09:24:47, 09:26:21, 09:29:03, 09:31:37), then it fell back to 400–900 MHz. Obs: samples2.log, 09:22–09:33.
- Synthetic test: `taskset -c 3` busy loop gave **cpu3 = 600 MHz** after 2.5 s. Obs: 09:21.
- The hot thread (tid 68047, hash phase) migrated cpu1 (P) → cpu12 (E) → cpu0 (P) → cpu17 (E) in 4 s, at 1.0–1.2 GHz throughout. Obs: /proc/<tid>/stat field 39, 09:21.
- Temperatures: package 63–66 °C (high 110 °C), cores ~60 °C, fans 6,342–6,821 RPM. Obs: sensors, 09:19–09:21.
- Throttle counters (cumulative since boot, uptime 1 h 40 min): cpu4 package_throttle_count 125,615 at 09:21 → 188,861 at 09:33. cpu0 package 17,084 → 17,762 in about 1 min. These events come in bursts (0 in one 10 s window). Obs: thermal_throttle sysfs.
- Power: `/sys/class/power_supply/ucsi-source-psy-USBC000:001` online, `usb_type PD_PPS`, **`voltage_now=5000000`, `current_now=3000000`**, current_max 3.25 A, voltage_max 20 V. AC online. BAT0 `Not charging`, capacity 99%, power_now 0. Obs: 09:22.

### Inferences
- The low clocks, throttle-counter bursts at low temperature and the 15 W input together suggest the embedded controller is enforcing a power or PROCHOT limit because the charger/cable negotiated only 5 V. This is not thermal throttling. The cause is likely, but MSR / turbostat confirmation needs root.
- Replacing the charger/cable with one that negotiates 20 V (≥65 W) would plausibly speed up the hash phase and msghand-bound phase by ~3–4×, and the script-check phase by a similar factor, since effective clocks would rise from ~0.9 GHz to ~3.5–4.5 GHz. This is an estimate, not a measurement.
- Thread migration onto E-cores also slows the single-threaded phases somewhat. It is minor next to the clamp.

### Gaps
- `turbostat` needs root and `perf_event_paranoid=4` blocks perf, so the throttle reason (PL1 / PROCHOT / EDP) and true effective clock (APERF/MPERF) were not read. Suggested check by the user: `sudo turbostat --quiet --show Busy%,Bzy_MHz,PkgWatt,CorWatt -i 5`, and `sudo rdmsr 0x64f` (core perf-limit reasons).

## 4. Config audit and proposed changes (proposals only; no repo files edited)

### Takeaway
Settings that affect sync speed: dbcache (capped at 2 GiB by code, while SPEC says up to 16 GiB), the container memory limit (compose default 2 g, this deployment 6 GiB), blocksonly=1 (good), blockfilterindex (default 1, here 0), par (auto, 15, fine), prune (here 10000), the networking pause during download (good), and profile replacement at `initialblockdownload=false` (good). The highest-value changes are a larger dbcache during snapshot load and tip sync with a matching memory limit, and an implementation that follows SPEC §2.3.2 again.

### Cited Findings
- `sync_dbcache()`: "at most half of current headroom, capped at 2 GiB", floor 64 MiB (`[ "$v" -gt 2048 ] && v=2048`). `steady_dbcache` caps BITCOIN_DBCACHE to the same value. — [images/bitcoind/sync-tuning.sh](file:///home/fmar/workspace/allod/images/bitcoind/sync-tuning.sh)
- `mem_mib()`: headroom = min(MemAvailable − reserve(25% of total, 512–4096 MiB), cgroup limit − current usage − 768 MiB). — [sync-tuning.sh](file:///home/fmar/workspace/allod/images/bitcoind/sync-tuning.sh)
- **SPEC conflicts with code:** SPEC says sync `dbcache` = "60% of the memory above 2 GiB (the container's cgroup limit counts if lower), between 450 MiB and 16 GiB and never below `BITCOIN_DBCACHE`", and records a live mainnet run that "gave `dbcache=16384`". — [SPEC.md §2.3.2 "Initial sync tuning"](file:///home/fmar/workspace/allod/SPEC.md) (line ~1126)
- `par=` is emitted only when a cgroup CPU quota is below nproc; bitcoind otherwise auto-selects. Here it logged 15 additional threads, the most v31 allows for 22 CPUs. — [sync-tuning.sh](file:///home/fmar/workspace/allod/images/bitcoind/sync-tuning.sh); Obs: debug.log line 74
- `watch_sync` polls every 60 s. When `initialblockdownload` is false it touches `.initial-sync-done` and stops bitcoind; restart writes the normal profile (blocksonly off, `BITCOIN_DBCACHE`). With AssumeUTXO, IBD ends when the snapshot chain reaches the tip, so the profile is held until the tip. — [sync-tuning.sh](file:///home/fmar/workspace/allod/images/bitcoind/sync-tuning.sh)
- Entrypoint writes `blockfilterindex=${BITCOIN_BLOCKFILTERINDEX:-1}`, `txindex=0`, `prune=$PRUNE` (auto: disk-sized, 5000–50000), sync profile then `dbcache`. — [images/bitcoind/bitcoind-entrypoint.sh](file:///home/fmar/workspace/allod/images/bitcoind/bitcoind-entrypoint.sh); [prune-size.sh](file:///home/fmar/workspace/allod/images/bitcoind/prune-size.sh)
- compose bitcoind: `mem_limit: ${BITCOIN_MEMORY_LIMIT:-2g}`, `memswap_limit` same, no CPU limit, `stop_grace_period: 5m`, named volumes `bitcoind-data`, `bitcoind-control`. `BITCOIN_DBCACHE` default 450, `BITCOIN_BLOCKFILTERINDEX` default 1. — [deploy/compose.yaml](file:///home/fmar/workspace/allod/deploy/compose.yaml), [deploy/.env.example](file:///home/fmar/workspace/allod/deploy/.env.example)
- assumeutxo.sh pauses the network (`setnetworkactive false`) once the base header is known during download, resumes after load, and only acts once Allod writes `/control/fastsync-request.json`. Live: pause 09:03:50, resume 09:21:59. — [images/bitcoind/assumeutxo.sh](file:///home/fmar/workspace/allod/images/bitcoind/assumeutxo.sh); Obs: debug.log
- Live deployment env: `BITCOIN_DBCACHE=2000`, `BITCOIN_PRUNE=10000`, `BITCOIN_BLOCKFILTERINDEX=0`, memory limit 6 GiB. Live conf: `dbcache=2048 blocksonly=1 blockfilterindex=0 prune=10000`. Obs: docker inspect env, /data/bitcoin.conf.

### Inferences / proposed diffs (for the report writer; not applied)

**P1: dbcache sizing in sync-tuning.sh (follow SPEC again; highest value).** At 2 GiB the snapshot load flushed 12 times (~265 s). After activation the cache restarts cold. A cache of ~12–16 GiB would hold the whole 164 M-coin snapshot without intermediate flushes, keep tip-sync lookups in RAM, and remove most of the 140–420 MB/s LevelDB reads.
```diff
 sync_dbcache() {
-  local mem=$1 v
-  v=$((mem / 2))
-  [ "$v" -gt 2048 ] && v=2048
-  [ "$v" -lt 64 ] && v=64
+  # SPEC §2.3.2: 60% of the memory above 2 GiB, 450 MiB..16 GiB, never below BITCOIN_DBCACHE.
+  local mem=$1 floor=${BITCOIN_DBCACHE:-450} v
+  v=$(( (mem - 2048) * 6 / 10 ))
+  [ "$v" -gt 16384 ] && v=16384
+  [ "$v" -lt 450 ] && v=450
+  [ "$v" -lt "$floor" ] && v=$floor
   echo "$v"
 }
```
In `mem_mib()`, the cgroup branch should also reserve page cache, not only 768 MiB. Measured anon at a 2 GiB dbcache was ~3.5 GB, i.e. ~1.4 GB beyond the cache. Suggest `cgroup_headroom=$((limit - used - 1536))` and dbcache ≤ (limit − 3 GiB) so ≥1.5 GiB remains for LevelDB page cache. Keep `steady_dbcache` on the old conservative cap (or a separate function) so the post-sync profile stays small.

**P2: memory limit in deploy/compose.yaml and .env.example.** The 2 g default (6 g here) caps P1. Compose cannot change the limit at runtime. Options:
- (a) Default `BITCOIN_MEMORY_LIMIT` to a host-sized value chosen by the installer (VPS/Umbrel/StartOS wrappers, ISO `allod-data-setup`), e.g. min(16g + 3g, 70% of RAM).
- (b) Add `mem_reservation`, or replace the hard limit with a soft one so page cache can grow, while keeping `memswap_limit` to prevent swap.
```diff
-    mem_limit: ${BITCOIN_MEMORY_LIMIT:-2g}
-    memswap_limit: ${BITCOIN_MEMORY_LIMIT:-2g}
+    # Sync needs dbcache + ~3 GiB (anon overhead + LevelDB page cache); see sync-tuning.sh.
+    mem_limit: ${BITCOIN_MEMORY_LIMIT:-2g}
+    memswap_limit: ${BITCOIN_MEMORY_LIMIT:-2g}
+    # The installer sets BITCOIN_MEMORY_LIMIT from host RAM (e.g. 19g on a 32 GB host) for the first sync.
```
Also update the `.env.example` comment ("max 2 GiB") to the SPEC formula.

**P3: keep `blocksonly=1` and the network pause.** Both behave as intended. The network was idle ~85% of the time (3.6 MB/s average against a ~30 MB/s link). No change needed.

**P4: blockfilterindex during the snapshot phase.** The default is 1 (here 0, so not measured). Consider writing `blockfilterindex=0` in the sync profile and enabling it in the normal profile, since the restart already happens at `initialblockdownload=false`. This avoids index I/O and CPU competing with validation. Needs a check that index building over the snapshot or background chain does not block LDK; not measured here (Gap).

**P5: par.** Leave it on auto. 15 is already v31's maximum script-check thread count, and there is no CPU quota. Pinning to P-cores (`cpuset`) is not recommended generally, because the E-cores add throughput during the script phase.

**P6: assumeutxo.sh / setup flow ordering.** The base header was known at 08:45:34, but fast sync was requested at ~09:03 (after ~20 min of genesis IBD). If the wizard offers fast sync as the default on mainnet, start the download right after headers sync. Optionally write `networkactive`-paused state earlier so blocks from genesis are not fetched first. Minor for time-to-tip: the genesis blocks are reused by background validation.

**P7: assumevalid (trust trade-off, not a default).** v31.1's assumevalid is at 938,343, so ~36 k post-snapshot blocks need full script checks, which is the CPU-bound phase. Setting `assumevalid=<recent hash>` in the sync profile would skip them but changes the trust model. Only mention it as an operator option.

**P8: host advice (ISO / docs).** Warn when CPU clocks are clamped (e.g. USB-C PD at 5 V). On this machine that, not Allod config, is the largest factor.

### Gaps
- The P1 gains (flush count, tip-sync blocks/s at 12–16 GiB dbcache) are estimated, not measured on this host.

## 5. Time-to-tip estimate

### Takeaway
At 10:00 the node needed ~34,800 more blocks (939,725 → ~974,511). At the measured post-assumevalid rate of 2.35–2.46 blocks/s that is **~3.9–4.1 h** (ETA ~14:00 UTC), plus a few minutes for cache-full flushes and the profile restart. From a fresh start with a fast-sync request the total is: download ~5 min + load 13.5 min + tip sync ~4.7 h (whole span at 2.07 blocks/s for 39.5 k blocks ≈ 5.3 h) ≈ **5–5.5 h** on this clamped CPU.

### Cited Findings
- Overall measured rate 2.07 blocks/s (09:22–10:00); 2.35–2.46 blocks/s after assumevalid. Headers 974,511 at 09:22. Obs: sampler, getpeerinfo/debug.log.
- Snapshot load wall time 09:08:22 → 09:21:56 = 13 min 34 s (coin load + 12 flushes 10 min 46 s; hash check 2 min 35 s). Obs: debug.log.

### Inferences
- After the stack removal at ~10:00:45, a restart will bring up a cold 2 GiB cache and the same clamped CPU, so expect the ~0.7 blocks/s phase again for the first several minutes.
- With a proper ≥65 W charger (clocks ~3.5–4.5 GHz) plus P1/P2 (≥12 GiB dbcache), tip sync could plausibly drop to roughly 1–1.5 h. This is an estimate based on the CPU-bound behaviour in phase C and the cache-miss behaviour in phases A/B, not a measurement.

### Gaps
- Rates late in the range (blocks in 2026 near the tip) were not observed. Block size and input count variance could shift the estimate by ±20%.
